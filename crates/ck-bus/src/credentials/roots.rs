//! The vault root keys ck-bus signs with, and how it notices one rotated.
//!
//! Root keys are created once by an operator ceremony (`ck auth mint-signing-key`) and
//! reached by credential id. Every credential-id name must come from
//! `cortexkit-bus-naming`, and at the pinned commons revision that crate has no
//! constructor for root credential ids, so resolving a root refuses with
//! `naming-constructor-absent` naming the missing constructor. ck-bus never writes the id
//! as a literal of its own. Callers that already hold an id (the acceptance harness's
//! fixture roots) pass it in directly.

use std::{
    collections::HashMap,
    fmt,
    sync::{Mutex, MutexGuard},
};

/// The recorded condition name for a name the naming crate cannot construct yet.
pub const NAMING_CONSTRUCTOR_ABSENT: &str = "naming-constructor-absent";

/// The roots the credential design uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RootCredential {
    /// Signs participant and ck-bus box-account user JWTs.
    BoxAccount,
    /// Signs ck-bus's system-account user JWT.
    SystemAccount,
    /// The box-local operator signing key: signs the two account JWTs, including every
    /// revocation claims update.
    Operator,
    /// Signs ck-bus's federation-account user JWT.
    FederationAccount,
    /// Signs per-message sender signatures for cross-machine deliveries.
    MessageSigning,
}

/// The naming crate cannot construct this root's credential id yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamingConstructorAbsent {
    pub root: RootCredential,
    pub constructor: &'static str,
}

impl fmt::Display for NamingConstructorAbsent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{NAMING_CONSTRUCTOR_ABSENT}: cortexkit-bus-naming has no {} for the {:?} root",
            self.constructor, self.root
        )
    }
}

impl RootCredential {
    /// The credential id to call the vault with.
    pub fn credential_id(self) -> Result<String, NamingConstructorAbsent> {
        Err(NamingConstructorAbsent {
            root: self,
            constructor: "root credential id constructor (signing:<provider>:<generation>)",
        })
    }
}

/// What a newly seen `key_id` means for a root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyIdObservation {
    /// The first `key_id` this process has seen for the root.
    First,
    Unchanged,
    /// The root was rotated (`mint-signing-key --replace`): every JWT it signed under
    /// `previous` must be re-issued.
    Rotated {
        previous: String,
    },
}

/// The `key_id` of every root this process signed with, kept in memory.
///
/// A rotation is detected by comparing `key_id`, never by `record_version`: the vault's
/// record version is not monotonic across a delete-and-remint or a restore.
#[derive(Debug, Default)]
pub struct KeyIdLedger {
    seen: Mutex<HashMap<String, String>>,
}

impl KeyIdLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `key_id` for `credential_id` and says whether it changed.
    pub fn observe(&self, credential_id: &str, key_id: &str) -> KeyIdObservation {
        let mut seen = self.lock();
        match seen.insert(credential_id.to_string(), key_id.to_string()) {
            None => KeyIdObservation::First,
            Some(previous) if previous == key_id => KeyIdObservation::Unchanged,
            Some(previous) => KeyIdObservation::Rotated { previous },
        }
    }

    pub fn current(&self, credential_id: &str) -> Option<String> {
        self.lock().get(credential_id).cloned()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, String>> {
        // Every mutation is a single insert, so a poisoned map is still consistent.
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
