use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeFingerprint(BTreeMap<PathBuf, String>);

pub fn operator_module_dir() -> Option<PathBuf> {
    let root = env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".local/share"))
        })?;
    Some(root.join("cortexkit/ckbus"))
}

pub fn fingerprint(root: Option<&Path>) -> TreeFingerprint {
    let mut entries = BTreeMap::new();
    if let Some(root) = root {
        fingerprint_path(root, root, &mut entries);
    }
    TreeFingerprint(entries)
}

fn fingerprint_path(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, String>) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    let relative = path.strip_prefix(root).unwrap_or(path).to_path_buf();
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let kind = if metadata.is_dir() {
        "dir"
    } else if metadata.file_type().is_symlink() {
        "symlink"
    } else {
        "file"
    };
    let digest = if metadata.is_file() {
        fs::read(path)
            .ok()
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
            .unwrap_or_else(|| "unreadable".to_string())
    } else {
        String::new()
    };
    entries.insert(
        relative,
        format!("{kind}:{}:{modified}:{digest}", metadata.len()),
    );
    if metadata.is_dir() {
        let mut children: Vec<_> = fs::read_dir(path)
            .unwrap_or_else(|error| {
                panic!(
                    "operator data-home observation failed at {}: {error}",
                    path.display()
                )
            })
            .map(|entry| {
                entry
                    .expect("operator data-home directory entry must be readable")
                    .path()
            })
            .collect();
        children.sort();
        for child in children {
            fingerprint_path(root, &child, entries);
        }
    }
}
