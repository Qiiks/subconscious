#![forbid(unsafe_code)]

use std::{path::PathBuf, process};

use cortexkit_log::{Config, Lane, Retention};

#[tokio::main]
async fn main() {
    // Side-effect-free provenance probes: evaluated before tracing, bootstrap, or
    // any runtime state so neither touches the start-lock nor reports an
    // already-running daemon.
    //
    // HELP MUST BE HANDLED HERE FOR THE SAME REASON --version IS. Without it, a help
    // request falls through into bootstrap and RUNS THE DAEMON STARTUP PATH: today
    // it stops at the singleton lock and logs "subc daemon already running", which
    // looks harmless and is safe only by CIRCUMSTANCE -- the circumstance being that
    // a daemon happens to be up. On a machine where none is, the same invocation
    // claims the start-lock, publishes a connection file and binds the port. An
    // operator asking a daemon binary what its flags are would start it.
    //
    // Scanned across all arguments rather than only the first, because the shape
    // someone types is a real invocation with the flag appended.
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if args.iter().any(|arg| arg == "--version") {
        println!("ck-subc {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    // PARSE-THEN-ACT: EVERY argument is settled before the first side effect, not
    // just the two recognised ones. Handling --help and --version early while
    // letting anything else fall through leaves the original defect for every OTHER
    // argument -- a typo, a flag copied from another tool, a stale invocation -- all
    // of which would silently START A DAEMON. The daemon takes no arguments, so the
    // complete rule is: recognise the two probes, refuse everything else, and only
    // then bootstrap.
    if let Some(unknown) = args
        .iter()
        .find(|arg| *arg != "--help" && *arg != "-h" && *arg != "help" && *arg != "--version")
    {
        eprintln!(
            "ck-subc: unexpected argument '{}'\n\nck-subc takes no arguments; \
             it is started by launchd and reads subc.jsonc from the XDG config \
             directory. Use `ck` to inspect or control a running daemon, or \
             `ck-subc --help`.",
            unknown.to_string_lossy()
        );
        process::exit(2);
    }
    if args
        .iter()
        .any(|arg| arg == "--help" || arg == "-h" || arg == "help")
    {
        println!(
            "ck-subc {} — the CortexKit subc daemon\n\n\
             Started by launchd; it takes no arguments and reads its configuration\n\
             from subc.jsonc under the XDG config directory.\n\n\
             flags:\n  \
               --version   print the version and exit\n  \
               --help      print this and exit\n\n\
             To inspect or control a running daemon use `ck` (`ck module list`,\n\
             `ck health`, `ck daemon`). Running this binary directly starts a daemon.",
            env!("CARGO_PKG_VERSION")
        );
        return;
    }

    if let Err(err) = init_tracing() {
        eprintln!("ck-subc: failed to initialize logging: {err}");
        process::exit(1);
    }

    if let Err(err) = subc_core::bootstrap::run().await {
        tracing::error!(error = %err, "subc-core failed");
        eprintln!("subc-core: {err}");
        process::exit(1);
    }
}

fn init_tracing() -> Result<(), cortexkit_log::InitError> {
    let config_path = subc_core::daemon_config::default_config_path();
    let logging = subc_core::daemon_config::load_logging(&config_path)
        .map_err(|error| {
            eprintln!(
                "ck-subc: could not read daemon logging config from {}: {error}; using defaults",
                config_path.display()
            );
        })
        .ok()
        .flatten();
    let logs_dir = subc_core::daemon_config::daemon_run_dir().join("logs");
    install_tracing(daemon_logger_config(logs_dir, logging.as_ref()))
}

fn daemon_logger_config(
    logs_dir: PathBuf,
    logging: Option<&subc_core::daemon_config::LoggingConfig>,
) -> Config {
    let path = logs_dir.join("subc.log");
    Config {
        module_id: "subc".to_string(),
        logs_dir,
        lane: Lane::Custom(path),
        spec: logging.map(subc_core::daemon_config::LoggingConfig::filter_spec),
        retention: logging.map_or_else(Retention::default, |config| config.retention),
        redactor: None,
        clock: None,
    }
}

fn install_tracing(config: Config) -> Result<(), cortexkit_log::InitError> {
    // cortexkit-log owns one process-global file sink and does not expose a
    // cheap tee layer. The daemon therefore writes directly to subc.log only;
    // stdout is intentionally not a second logging destination.
    cortexkit_log::init(config).map(|_| ())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::*;

    #[test]
    fn daemon_log_line_matches_the_authority_fixture_byte_for_byte_without_ansi() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let logs_dir =
            std::env::temp_dir().join(format!("subc-daemon-log-format-{}-{unique}", process::id()));
        let mut config = daemon_logger_config(logs_dir.clone(), None);
        config.module_id = "fusiform".to_string();
        config.clock = Some(Arc::new(|| {
            UNIX_EPOCH + Duration::from_millis(1_788_604_863_123)
        }));
        install_tracing(config).unwrap();

        tracing::info!(
            version = 1_788_526_509_641_u64,
            eras = 22_u64,
            facts_changed = 0_u64,
            arrived = 2_u64,
            "poll changed"
        );
        let line = fs::read_to_string(logs_dir.join("subc.log")).unwrap();
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/log_format_golden.json")).unwrap();
        let expected = fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == "plain-info-no-session")
            .unwrap()["line"]
            .as_str()
            .unwrap();
        assert_eq!(line, format!("{expected}\n"));
        assert!(!line.contains('\u{1b}'));
    }
}
