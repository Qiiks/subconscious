use std::{
    fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use subc_core::test_support::TestTempDir;

struct Fixture {
    root: TestTempDir,
    child: Option<Child>,
}

impl Fixture {
    fn new() -> Self {
        let root = TestTempDir::new("durable-terminals");
        fs::create_dir_all(root.join("config/cortexkit")).unwrap();
        fs::create_dir_all(root.join("runtime")).unwrap();
        fs::create_dir_all(root.join("data/cortexkit/run")).unwrap();
        fs::write(
            root.join("config/cortexkit/subc.jsonc"),
            serde_json::to_vec(&json!({
                "version": 1,
                "modules": {
                    "history": {
                        "program": env!("CARGO_BIN_EXE_fake-aft-stub"),
                        "enabled": false,
                        "drain_timeout_ms": 25,
                        "env": { "FAKE_AFT_MODULE_ID": "history" }
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        Self { root, child: None }
    }

    fn journal(&self) -> PathBuf {
        self.root.join("data/cortexkit/run/terminals.jsonl")
    }

    fn connection(&self) -> PathBuf {
        self.root
            .join("runtime")
            .join(subc_transport::CONNECTION_FILE_NAME)
    }

    fn boot(&mut self) {
        self.child = Some(
            Command::new(env!("CARGO_BIN_EXE_ck-subc"))
                .env("XDG_DATA_HOME", self.root.join("data"))
                .env("XDG_CONFIG_HOME", self.root.join("config"))
                .env("XDG_RUNTIME_DIR", self.root.join("runtime"))
                .env("SUBC_PORT", "0")
                .env_remove("CK_LOG")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                panic!("daemon did not boot: {status}");
            }
            if self.connection().exists()
                && self
                    .try_ck(&["module", "terminals", "history", "--json"])
                    .is_some()
            {
                break;
            }
            if Instant::now() >= deadline {
                panic!("daemon never became readable");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_file(self.connection());
    }

    fn try_ck(&self, args: &[&str]) -> Option<String> {
        let output = Command::new(env!("CARGO_BIN_EXE_ck"))
            .arg("--subc")
            .arg(self.connection())
            .args(args)
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8(output.stdout).unwrap())
    }

    fn ck(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.try_ck(args).expect("ck command succeeds")).unwrap()
    }

    fn terminals(&self) -> Value {
        self.ck(&["module", "terminals", "history", "--json"])
    }

    fn exit(&self) {
        self.ck(&["module", "start", "history", "--json"]);
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.ck(&["module", "status", "history", "--json"])["module"]["live"] != true {
            if Instant::now() >= deadline {
                panic!("module never registered");
            }
            thread::sleep(Duration::from_millis(10));
        }
        self.ck(&["module", "stop", "history", "--json"]);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.kill();
    }
}

#[test]
fn daemon_restart_recovers_terminal_when_the_ring_is_empty() {
    let mut fixture = Fixture::new();
    fixture.boot();
    fixture.exit();
    let before = fixture.terminals()["entries"].as_array().unwrap().clone();
    fixture.kill();
    fixture.boot();
    let after = fixture.terminals()["entries"].as_array().unwrap().clone();
    assert_eq!((before.len(), after), (1, before));
}

#[test]
fn daemon_restart_distinguishes_incarnations_on_both_sides() {
    let mut fixture = Fixture::new();
    fixture.boot();
    fixture.exit();
    fixture.kill();
    fixture.boot();
    fixture.exit();
    let history = fixture.terminals();
    let entries = history["entries"].as_array().unwrap();
    let incarnations = entries
        .iter()
        .map(|entry| entry["daemon_incarnation"].as_str().unwrap_or(""))
        .collect::<Vec<_>>();
    assert!(
        incarnations.len() == 2
            && !incarnations[0].is_empty()
            && !incarnations[1].is_empty()
            && incarnations[0] != incarnations[1],
        "distinct daemon lifetimes must remain distinguishable: {incarnations:?}"
    );
}

#[test]
fn daemon_boots_with_missing_terminal_journal() {
    let mut fixture = Fixture::new();
    fixture.boot();
    assert_eq!(fixture.terminals()["entries"], json!([]));
}

#[test]
fn daemon_boots_with_empty_terminal_journal() {
    let mut fixture = Fixture::new();
    fs::write(fixture.journal(), b"").unwrap();
    fixture.boot();
    assert_eq!(fixture.terminals()["entries"], json!([]));
}

#[test]
fn daemon_boots_with_unreadable_terminal_journal_and_reports_it() {
    let mut fixture = Fixture::new();
    // A directory is unreadable as a journal on Unix and Windows, including
    // privileged test runners for whom mode 000 would still be readable.
    fs::create_dir(fixture.journal()).unwrap();
    fixture.boot();
    let history = fixture.terminals();
    assert_eq!(
        (
            history["entries"].clone(),
            history["journal_read_errors"].clone()
        ),
        (json!([]), json!(1))
    );
}

#[test]
fn daemon_boots_with_garbage_terminal_journal_and_reports_skipped_lines() {
    let mut fixture = Fixture::new();
    fs::write(fixture.journal(), b"garbage\n\xff\n{\"partial\":").unwrap();
    fixture.boot();
    let history = fixture.terminals();
    assert_eq!(
        (
            history["entries"].clone(),
            history["journal_skipped_lines"].clone()
        ),
        (json!([]), json!(3))
    );
}

#[test]
fn cli_renders_the_incarnation_and_corruption_warning() {
    let mut fixture = Fixture::new();
    fs::write(fixture.journal(), b"bad line\n").unwrap();
    fixture.boot();
    fixture.exit();
    let history = fixture.terminals();
    let incarnation = history["entries"][0]["daemon_incarnation"]
        .as_str()
        .unwrap();
    let text = fixture.try_ck(&["module", "terminals", "history"]).unwrap();
    assert!(
        text.contains(incarnation)
            && text.contains("warning: 1 unreadable terminal journal lines skipped"),
        "{text}"
    );
}
