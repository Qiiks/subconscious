//! Credentials: vault roots, per-process keys held in memory, and the user JWTs ck-bus
//! builds and has a root sign.
//!
//! Under the credential design the vault holds only root keys, made by an operator
//! ceremony. ck-bus generates every per-process user key in its own memory, builds the
//! user JWT with the generated grant, and has the right root sign it through
//! `credential.sign`. It reads a root's public half through `credential.public_key` and
//! builds every NATS encoding (nkey string, JWT) itself. The vault is never asked for a
//! secret half.
//!
//! This area's seam is `VaultSigning`: production implements it over a subc route to
//! `claustrum` carrying ck-bus's consumer identity, and acceptance serves the same wire
//! from throwaway keys.

pub mod custody;
pub mod issue;
pub mod jwt;
pub mod nkey;
pub mod roots;
pub mod vault;
pub mod wire;

use std::sync::Arc;

use custody::KeyCustody;
use roots::KeyIdLedger;
use vault::{ClaustrumRoute, VaultSigning};

/// The credentials area as wired at start: the vault route, the in-memory key custody,
/// and the `key_id` of every root signed with.
pub struct Credentials {
    pub vault: Arc<dyn VaultSigning>,
    pub custody: KeyCustody,
    pub key_ids: KeyIdLedger,
}

impl Credentials {
    pub fn new(vault: Arc<dyn VaultSigning>) -> Self {
        Self {
            vault,
            custody: KeyCustody::new(),
            key_ids: KeyIdLedger::new(),
        }
    }

    /// The area for a supervised ck-bus, reaching the vault over its own subc route. No
    /// I/O happens until the first vault call.
    pub fn supervised() -> Result<Self, String> {
        Ok(Self::new(Arc::new(ClaustrumRoute::supervised()?)))
    }
}
