//! Seed absence on enumerated surfaces: a process's environment and argv, a directory
//! tree (the store root, the capture logs), and a report text. The pattern is the nkey
//! seed encoding, `S[UAOCN][A-Z2-7]{54}`.

use std::path::{Path, PathBuf};

/// Every substring of `text` shaped like an nkey seed.
pub fn find_seeds(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let base32 = |b: u8| b.is_ascii_uppercase() || (b'2'..=b'7').contains(&b);
    let mut found = Vec::new();
    for start in 0..bytes.len().saturating_sub(55) {
        if bytes[start] == b'S'
            && b"UAOCN".contains(&bytes[start + 1])
            && bytes[start + 2..start + 56].iter().all(|b| base32(*b))
        {
            found.push(text[start..start + 56].to_string());
        }
    }
    found
}

/// Seeds found in any file under `root`, with the file they were found in.
pub fn scan_tree(root: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                pending.extend(entries.filter_map(|entry| entry.ok().map(|e| e.path())));
            }
        } else if metadata.is_file() {
            if let Ok(bytes) = std::fs::read(&path) {
                for seed in find_seeds(&String::from_utf8_lossy(&bytes)) {
                    found.push((path.clone(), seed));
                }
            }
        }
    }
    found
}

/// A live process's environment as text: `/proc/<pid>/environ` on Linux, `ps eww` on
/// macOS (which prints the command followed by the environment).
pub fn process_environment(pid: u32) -> String {
    #[cfg(target_os = "linux")]
    {
        let bytes = std::fs::read(format!("/proc/{pid}/environ"))
            .unwrap_or_else(|error| panic!("read /proc/{pid}/environ: {error}"));
        String::from_utf8_lossy(&bytes).replace('\0', "\n")
    }
    #[cfg(not(target_os = "linux"))]
    {
        ps(pid, &["eww", "-o", "command="])
    }
}

/// A live process's argv, by the portable `ps -ww -o args= -p <pid>`.
pub fn process_argv(pid: u32) -> String {
    ps(pid, &["-ww", "-o", "args="])
}

fn ps(pid: u32, args: &[&str]) -> String {
    let output = std::process::Command::new("ps")
        .args(args)
        .arg("-p")
        .arg(pid.to_string())
        .output()
        .expect("run ps");
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert!(
        output.status.success() && !text.is_empty(),
        "ps {args:?} -p {pid} read nothing (exit {}); the scan would prove nothing",
        output.status
    );
    text
}

/// Reads one variable out of `process_environment` text.
pub fn environment_value(environment: &str, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    environment
        .split(|c: char| c == '\n' || c.is_whitespace())
        .find_map(|token| token.strip_prefix(&prefix).map(str::to_string))
}
