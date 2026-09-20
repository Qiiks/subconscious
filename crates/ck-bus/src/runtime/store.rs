use std::{env, error::Error, fmt, path::PathBuf};

use crate::MODULE_ID;

#[derive(Debug)]
pub struct StoreRootError {
    message: String,
}

impl StoreRootError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for StoreRootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for StoreRootError {}

/// Resolve the module directory with the shared environment-based storage-path rules.
pub fn resolve_store_root() -> Result<PathBuf, StoreRootError> {
    #[cfg(unix)]
    const ROOT_ENVIRONMENT_KEYS: &[&str] = &["XDG_DATA_HOME", "HOME"];
    #[cfg(windows)]
    const ROOT_ENVIRONMENT_KEYS: &[&str] = &[
        "XDG_DATA_HOME",
        "HOME",
        "LOCALAPPDATA",
        "APPDATA",
        "USERPROFILE",
    ];
    #[cfg(not(any(unix, windows)))]
    const ROOT_ENVIRONMENT_KEYS: &[&str] = &["XDG_DATA_HOME", "HOME"];

    let has_environment_root = ROOT_ENVIRONMENT_KEYS
        .iter()
        .any(|key| env::var_os(key).is_some_and(|value| !value.is_empty()));
    if !has_environment_root {
        return Err(StoreRootError::new(
            "ck-bus refuses a store root without XDG_DATA_HOME or a platform home variable",
        ));
    }

    let root = PathBuf::from(cortexkit_store_types::module_data_dir(MODULE_ID));
    if !root.is_absolute() {
        return Err(StoreRootError::new(format!(
            "ck-bus refuses relative module data directory {}",
            root.display()
        )));
    }
    Ok(root)
}
