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
    if let Some(path) = env::var_os("LOG_CHILD_ENV_PATH").map(PathBuf::from) {
        let ck_log = env::var("CK_LOG").ok();
        fs::write(
            path,
            ck_log.map_or_else(|| "absent".to_string(), |value| format!("present:{value}")),
        )
        .expect("write fixture environment observation");
    }
}
