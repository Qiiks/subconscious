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
        // One line per knob the daemon owes the child, so a test can assert each
        // by name and an absent one reads as "absent" rather than as nothing.
        let knob = |name: &str| {
            env::var(name).map_or_else(|_| "absent".to_string(), |value| format!("present:{value}"))
        };
        let observation = format!(
            "{}\n{}\n{}",
            knob("CK_LOG"),
            knob("CK_LOG_MAX_AGE_DAYS"),
            knob("CK_LOG_ALARM_SEGMENT_MB")
        );

        // WRITE-THEN-RENAME, so a reader sees either NO FILE or the COMPLETE
        // one. `fs::write` creates the file and then fills it, and a reader
        // landing between those two steps gets an EMPTY file that exists.
        //
        // That is not hypothetical: the Windows CI leg failed on
        // `left: ""  right: "absent"` (2026-09-19). The waiting test polled
        // `path.exists()`, which became true at creation, and read before the
        // content landed.
        //
        // Fixing it here rather than by strengthening the poll to "non-empty"
        // REMOVES THE RACE instead of narrowing it -- a partial write is still
        // non-empty, so that poll would have failed less often and lied the same
        // way. It also means `exists()` implies complete for every current and
        // future reader of this fixture, so the test's predicate and its
        // assertion agree by construction rather than by matching edits.
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, &observation).expect("write fixture environment observation");
        fs::rename(&tmp, &path).expect("publish fixture environment observation");
    }
}
