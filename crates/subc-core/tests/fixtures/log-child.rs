#![forbid(unsafe_code)]

use std::{env, fs, io::Write, path::PathBuf};

fn main() {
    let stdout_line = env::var("LOG_CHILD_STDOUT").unwrap_or_default();
    let stderr_line = env::var("LOG_CHILD_STDERR").unwrap_or_default();
    if !stdout_line.is_empty() {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{stdout_line}").expect("write fixture stdout");
    }
    if !stderr_line.is_empty() {
        let mut stderr = std::io::stderr().lock();
        writeln!(stderr, "{stderr_line}").expect("write fixture stderr");
    }
    // Both pipes writing at once is the case that can tear a line, and a
    // supervisor merging them into one file is exactly where it would show.
    // Writing them sequentially above cannot tear however the forwarder is
    // implemented, so that arm proves delivery and nothing about framing.
    let burst: usize = env::var("LOG_CHILD_BURST")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    if burst > 0 {
        let out = std::thread::spawn(move || {
            let mut stdout = std::io::stdout().lock();
            for i in 0..burst {
                writeln!(stdout, "out-{i:04}-{}", "o".repeat(64)).expect("burst stdout");
            }
        });
        let err = std::thread::spawn(move || {
            let mut stderr = std::io::stderr().lock();
            for i in 0..burst {
                writeln!(stderr, "err-{i:04}-{}", "e".repeat(64)).expect("burst stderr");
            }
        });
        out.join().expect("stdout burst thread");
        err.join().expect("stderr burst thread");
    }

    if let Some(path) = env::var_os("LOG_CHILD_ENV_PATH").map(PathBuf::from) {
        let ck_log = env::var("CK_LOG").ok();
        fs::write(
            path,
            ck_log.map_or_else(|| "absent".to_string(), |value| format!("present:{value}")),
        )
        .expect("write fixture environment observation");
    }
}
