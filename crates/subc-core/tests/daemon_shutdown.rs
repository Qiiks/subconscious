#![cfg(unix)]

use std::{
    fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use subc_daemon::{read_frame, test_support::TestTempDir, write_frame, Frame};
use subc_protocol::{Flags, FrameType, Priority};

mod common;

// Real daemons compete with other integration binaries for spawn/registration
// resources. Serialize this file instead of widening their startup deadlines.
static DAEMON_GATE: Mutex<()> = Mutex::new(());

struct Fixture {
    root: TestTempDir,
    child: Child,
    _permit: MutexGuard<'static, ()>,
}

impl Fixture {
    fn boot(busy: bool) -> Self {
        let permit = DAEMON_GATE.lock().unwrap_or_else(|p| p.into_inner());
        let root = TestTempDir::new("daemon-shutdown");
        for dir in ["config/cortexkit", "runtime", "data/cortexkit/run"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("config/cortexkit/subc.jsonc"), serde_json::to_vec(&json!({
            "version": 1,
            "modules": { "shutdown-observer": {
                "program": env!("CARGO_BIN_EXE_fake-aft-stub"),
                "env": {
                    "FAKE_AFT_MODULE_ID": "shutdown-observer",
                    "FAKE_AFT_EVENTS_PATH": root.join("events.jsonl"),
                    "FAKE_AFT_RECORD_EOF": "1",
                    "FAKE_AFT_DELAY_FROM_BODY": "1",
                    "FAKE_AFT_SHUTDOWN_JOURNAL": root.join("data/cortexkit/run/terminals.jsonl"),
                    "FAKE_AFT_BUSY_GAUGES": "work",
                    "FAKE_AFT_HEALTH_METRICS": if busy { "{\"work\":1}" } else { "{\"work\":0}" }
                }
            }}
        })).unwrap()).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_ck-subc"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_RUNTIME_DIR", root.join("runtime"))
            .env("SUBC_PORT", "0")
            .env_remove("CK_LOG")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut fixture = Self {
            root,
            child,
            _permit: permit,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                fixture.child.try_wait().unwrap().is_none(),
                "daemon exited during startup"
            );
            if fixture.connection().exists() {
                let output = Command::new(env!("CARGO_BIN_EXE_ck"))
                    .arg("--subc")
                    .arg(fixture.connection())
                    .args(["module", "status", "shutdown-observer", "--json"])
                    .output()
                    .unwrap();
                if serde_json::from_slice::<Value>(&output.stdout)
                    .is_ok_and(|status| status["module"]["live"] == true)
                {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "module did not register");
            thread::sleep(Duration::from_millis(10));
        }
        fixture
    }

    fn connection(&self) -> PathBuf {
        self.root
            .join("runtime")
            .join(subc_transport::CONNECTION_FILE_NAME)
    }

    fn term(&self) {
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(self.child.id() as i32).unwrap(),
            rustix::process::Signal::TERM,
        )
        .unwrap();
    }

    fn events(&self) -> Vec<Value> {
        fs::read_to_string(self.root.join("events.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    fn wait_event(&self, kind: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            if let Some(event) = self
                .events()
                .into_iter()
                .find(|event| event["kind"] == kind)
            {
                return event;
            }
            assert!(
                Instant::now() < deadline,
                "module never observed {kind}: {:?}",
                self.events()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_exit(&mut self, budget: Duration) {
        let deadline = Instant::now() + budget;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "SIGTERM must run the handler, not kill by default: {status}"
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "daemon exceeded its shutdown budget"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn provider_observes_draining_before_its_own_eof() {
    let mut fixture = Fixture::boot(false);
    fixture.term();
    fixture.wait_exit(Duration::from_secs(4));
    fixture.wait_event("eof");
    let sequence: Vec<_> = fixture
        .events()
        .into_iter()
        .filter_map(|e| e["kind"].as_str().map(str::to_owned))
        .filter(|kind| kind == "draining" || kind == "eof")
        .collect();
    assert_eq!(
        sequence,
        ["draining", "eof"],
        "provider must observe notice before losing its wire"
    );
}

#[test]
fn shutdown_marker_is_durable_and_precedes_provider_notice() {
    let mut fixture = Fixture::boot(false);
    let incarnation = subc_transport::read_for_client(fixture.connection())
        .unwrap()
        .daemon_id;
    fixture.term();
    fixture.wait_exit(Duration::from_secs(4));
    assert_eq!(fixture.wait_event("draining")["shutdown_marker_seen"], true);
    let journal =
        fs::read_to_string(fixture.root.join("data/cortexkit/run/terminals.jsonl")).unwrap();
    let markers: Vec<Value> = journal
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|entry| entry["event"] == "daemon_shutdown")
        .collect();
    assert_eq!(markers.len(), 1);
    assert_eq!(
        markers[0]["daemon_incarnation"],
        format!("{:032x}", u128::from_be_bytes(incarnation))
    );
    assert!(markers[0]["at_ms"].as_u64().unwrap() > 0);
}

#[test]
fn stubborn_provider_gets_a_bounded_drain_not_a_completion_promise() {
    let mut fixture = Fixture::boot(true);
    let started = Instant::now();
    fixture.term();
    fixture.wait_exit(Duration::from_secs(4));
    assert!(
        started.elapsed() >= Duration::from_millis(1800),
        "busy provider did not get its drain wait"
    );
}

#[test]
fn quiescent_provider_does_not_spend_the_whole_drain_budget() {
    let mut fixture = Fixture::boot(false);
    fixture.term();
    fixture.wait_exit(Duration::from_millis(1500));
}

#[test]
fn second_sigterm_cuts_short_a_stubborn_provider_wait() {
    let mut fixture = Fixture::boot(true);
    fixture.term();
    fixture.wait_event("draining");
    assert!(fixture.child.try_wait().unwrap().is_none());
    fixture.term();
    fixture.wait_exit(Duration::from_millis(800));
}

async fn open_route(fixture: &Fixture) -> (tokio::net::TcpStream, u16, u32) {
    let mut stream = common::connect_authed_client(fixture.connection())
        .await
        .unwrap();
    let request = json!({"op":"route.open", "target":{"kind":"tool_provider", "module_id":"shutdown-observer"},
        "identity":{"project_root":fixture.root.to_path_buf(), "harness":"shutdown-test", "session":"shutdown-test"}});
    write_frame(
        &mut stream,
        &Frame::build(
            FrameType::Request,
            Flags::new(false, Priority::Interactive, false),
            0,
            0,
            1,
            serde_json::to_vec(&request).unwrap(),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(4), read_frame(&mut stream))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        response.header.ty,
        FrameType::Response,
        "route open failed: {:?}",
        response.body
    );
    let body: Value = serde_json::from_slice(&response.body).unwrap();
    (
        stream,
        body["route_channel"].as_u64().unwrap() as u16,
        body["route_epoch"].as_u64().unwrap() as u32,
    )
}

#[tokio::test]
async fn consumer_observes_route_closing_on_channel_zero_before_eof() {
    let mut fixture = Fixture::boot(false);
    let (mut stream, _, _) = open_route(&fixture).await;
    fixture.term();
    let mut sequence = Vec::new();
    tokio::time::timeout(Duration::from_secs(4), async {
        while let Some(frame) = read_frame(&mut stream).await.unwrap() {
            if serde_json::from_slice::<Value>(&frame.body)
                .is_ok_and(|body| body["op"] == "route.closing")
            {
                assert_eq!(frame.header.channel, 0);
                sequence.push("route.closing");
            }
        }
        sequence.push("eof");
    })
    .await
    .unwrap();
    assert_eq!(sequence, ["route.closing", "eof"]);
    fixture.wait_exit(Duration::from_secs(1));
}

#[tokio::test]
async fn in_flight_reply_can_finish_during_the_shutdown_drain() {
    let mut fixture = Fixture::boot(false);
    let (mut stream, channel, epoch) = open_route(&fixture).await;
    write_frame(
        &mut stream,
        &Frame::build(
            FrameType::Request,
            Flags::new(false, Priority::Interactive, false),
            channel,
            epoch,
            2,
            br#"{"delay_ms":750}"#.to_vec(),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    fixture.wait_event("request_received");
    fixture.term();
    let mut sequence = Vec::new();
    tokio::time::timeout(Duration::from_secs(4), async {
        while let Some(frame) = read_frame(&mut stream).await.unwrap() {
            if frame.header.ty == FrameType::Response && frame.header.corr == 2 {
                sequence.push("reply");
            }
        }
        sequence.push("eof");
    })
    .await
    .unwrap();
    assert_eq!(sequence, ["reply", "eof"]);
    fixture.wait_exit(Duration::from_secs(1));
}
