//! The machine id: one opaque name per machine, owned by the daemon.

use std::{error::Error, fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// This machine's name, minted once by the daemon and served to every module
/// that registers with it (on `HELLO_ACK` and `server.describe`).
///
/// It is 16 random bytes rendered as exactly 32 lowercase hex characters. It is
/// opaque: it carries no structure and is not derived from a key, a hostname or
/// any other host fact. The daemon stores it at `<data home>/cortexkit/machine-id`
/// and never rewrites that file; an operator changes it only with
/// `ck machine adopt`, which takes effect at the next daemon start.
///
/// # A name, never an authority
///
/// Nothing may admit a peer, grant trust or skip a check because two messages
/// carry the same machine id. Authority stays on keys (a peer's roster entry, the
/// vault). The id deliberately outlives a key rotation, so treating it as an
/// identity would let a revoked key's history vouch for its replacement. Two
/// machines restored from one backup also carry the same id, which is exactly
/// why it can name a machine but never prove one.
///
/// Construction validates the shape, so a value of this type is always 32
/// lowercase hex characters. A module talking to a daemon that predates the id
/// sees no value at all and must read that as "the daemon predates the machine
/// id", never as "there is no machine" and never as a cue to mint its own.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MachineId(String);

impl MachineId {
    /// Length of the rendered id in characters (16 bytes, two hex digits each).
    pub const HEX_LEN: usize = 32;

    /// Validate `value` as a machine id: exactly 32 characters, each `0-9` or
    /// `a-f`. Uppercase hex is refused rather than folded, so one machine has
    /// exactly one spelling and string comparison is identity of the name.
    pub fn parse(value: &str) -> Result<Self, MachineIdError> {
        if value.len() != Self::HEX_LEN {
            return Err(MachineIdError::WrongLength { len: value.len() });
        }
        if let Some(position) = value
            .bytes()
            .position(|byte| !matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(MachineIdError::NotLowercaseHex { position });
        }
        Ok(Self(value.to_owned()))
    }

    /// Render 16 bytes as a machine id. The caller supplies the randomness; the
    /// daemon uses the operating system's CSPRNG.
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(format!("{:032x}", u128::from_be_bytes(bytes)))
    }

    /// The 32-character lowercase hex rendering.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MachineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for MachineId {
    type Err = MachineIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<String> for MachineId {
    type Error = MachineIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<MachineId> for String {
    fn from(value: MachineId) -> Self {
        value.0
    }
}

/// Why a string is not a machine id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineIdError {
    /// The value is not exactly [`MachineId::HEX_LEN`] bytes long.
    WrongLength { len: usize },
    /// The byte at `position` is not a lowercase hex digit.
    NotLowercaseHex { position: usize },
}

impl fmt::Display for MachineIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength { len } => write!(
                f,
                "a machine id is exactly {} lowercase hex characters, got {len} bytes",
                MachineId::HEX_LEN
            ),
            Self::NotLowercaseHex { position } => write!(
                f,
                "a machine id is exactly {} lowercase hex characters; byte {position} is not 0-9 or a-f",
                MachineId::HEX_LEN
            ),
        }
    }
}

impl Error for MachineIdError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_exactly_32_lowercase_hex() {
        let id = MachineId::parse("0123456789abcdef0123456789abcdef").expect("valid");
        assert_eq!(id.as_str(), "0123456789abcdef0123456789abcdef");
    }

    #[test]
    fn refuses_wrong_length_uppercase_and_non_hex() {
        assert_eq!(
            MachineId::parse("abc"),
            Err(MachineIdError::WrongLength { len: 3 })
        );
        assert_eq!(
            MachineId::parse("0123456789abcdef0123456789abcdef\n"),
            Err(MachineIdError::WrongLength { len: 33 })
        );
        assert_eq!(
            MachineId::parse("0123456789ABCDEF0123456789abcdef"),
            Err(MachineIdError::NotLowercaseHex { position: 10 })
        );
        assert_eq!(
            MachineId::parse("0123456789abcdeg0123456789abcdef"),
            Err(MachineIdError::NotLowercaseHex { position: 15 })
        );
    }

    #[test]
    fn from_bytes_renders_32_lowercase_hex_with_leading_zeros() {
        let mut bytes = [0u8; 16];
        bytes[15] = 0xab;
        let id = MachineId::from_bytes(bytes);
        assert_eq!(id.as_str(), "000000000000000000000000000000ab");
        assert_eq!(MachineId::parse(id.as_str()), Ok(id));
    }

    #[test]
    fn serde_refuses_a_malformed_value() {
        let ok: MachineId =
            serde_json::from_str("\"0123456789abcdef0123456789abcdef\"").expect("valid");
        assert_eq!(
            serde_json::to_string(&ok).unwrap(),
            "\"0123456789abcdef0123456789abcdef\""
        );
        assert!(serde_json::from_str::<MachineId>("\"not-an-id\"").is_err());
    }
}
