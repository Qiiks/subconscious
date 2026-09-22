#[allow(dead_code)]
mod harness;
#[cfg(unix)]
#[allow(dead_code)]
#[path = "support/mod.rs"]
mod support;

#[cfg(unix)]
use harness::{daemon::AcceptanceRun, data_home};
#[cfg(unix)]
use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a1_argv_non_injection() {
    let _gate = harness::acceptance_gate().await;
    harness::install_tracing();
    support::version_floor();
    let args = support::declared_args(support::SERVER);
    let operator = data_home::operator_module_dir();
    let before = data_home::fingerprint(operator.as_deref());
    let run = AcceptanceRun::start(Path::new(env!("CARGO_BIN_EXE_ck-bus"))).await;
    if let Some(dir) = &operator {
        assert!(
            !dir.starts_with(run.root.path()),
            "operator data home must be outside fixture"
        );
    }
    support::enable(&run, support::SERVER).await;
    let pid = support::pid(&run, support::SERVER).await;
    // BootstrapConfig::new receives this required path; reading it proves the daemon wrote the effective connection file in this run.
    let connection_file_path = &run.connection_file;
    assert!(
        connection_file_path.is_absolute()
            && subc_transport::connection_file::read(connection_file_path).is_ok(),
        "effective connection_file_path must exist in this run"
    );
    eprintln!(
        "A1 effective connection_file_path={}",
        connection_file_path.display()
    );
    let output = Command::new("ps")
        .args(["-ww", "-o", "args=", "-p", &pid.to_string()])
        .output()
        .expect("ps must execute");
    assert!(output.status.success(), "ps -ww must succeed: {:?}", output);
    let raw = String::from_utf8(output.stdout).expect("ps argv must be utf8");
    eprintln!("A1 ps -ww -o args= -p {pid}: {raw:?}");
    assert!(!raw.trim().is_empty(), "ps -ww argv must not be empty");
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    assert_eq!(
        tokens.last().copied(),
        args.last().map(String::as_str),
        "ps -ww must include final declared argument"
    );
    assert!(
        !tokens.iter().any(|token| token.starts_with("--subc")),
        "protocol none must suppress --subc: {raw:?}"
    );
    assert_eq!(
        tokens.len(),
        args.len() + 1,
        "ps tokens must match declared argv"
    );
    let configured: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&run.config_file).unwrap()).unwrap();
    let executable = configured["modules"][support::SERVER]["program"]
        .as_str()
        .expect("program path");
    let observed =
        std::fs::canonicalize(PathBuf::from(tokens[0])).expect("observed executable path");
    let declared = std::fs::canonicalize(executable).expect("declared executable path");
    assert_eq!(
        observed, declared,
        "argv[0] must match declared program after path normalization"
    );
    assert_eq!(
        &tokens[1..],
        args.iter().map(String::as_str).collect::<Vec<_>>(),
        "every later argv token must match declared args byte-exact"
    );
    run.shutdown().await;
    assert_eq!(
        data_home::fingerprint(operator.as_deref()),
        before,
        "operator data home must remain unchanged"
    );
    harness::report::RowReport::passed(harness::report::Row::SupervisedServerArgv)
        .validate(&Default::default())
        .unwrap();
}

#[cfg(not(unix))]
#[test]
fn a1_argv_non_injection() {
    let report = harness::report::RowReport::skipped(
        harness::report::Row::SupervisedServerArgv,
        "a1-signal-unix-only",
        "non-unix host",
    );
    report.validate(&Default::default()).unwrap();
    eprintln!("a1-signal-unix-only: non-unix host");
}
