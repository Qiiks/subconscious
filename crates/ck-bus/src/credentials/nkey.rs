//! NATS nkey public-key strings, built from the raw 32 bytes the vault returns.
//!
//! The vault serves `public_key_hex`, not an nkey, so ck-bus does the encoding: one role
//! prefix byte, the 32 key bytes, and a CRC-16/XMODEM of those 33 bytes in little-endian
//! order, all in unpadded RFC 4648 base32. The layout is the nats-io nkeys one; the unit
//! tests check this encoder against the `nkeys` crate's own output.

use data_encoding::BASE32_NOPAD;

/// The roles ck-bus names keys for. Each prefix byte makes the first base32 character
/// the role letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NkeyRole {
    /// `O`: signs account JWTs.
    Operator,
    /// `A`: an account identity or account signing key; signs user JWTs.
    Account,
    /// `U`: a user.
    User,
}

impl NkeyRole {
    const fn prefix_byte(self) -> u8 {
        match self {
            Self::Operator => 14 << 3,
            Self::Account => 0,
            Self::User => 20 << 3,
        }
    }
}

/// The nkey string for a raw Ed25519 public key in `role`.
pub fn encode_public(role: NkeyRole, public: &[u8; 32]) -> String {
    let mut raw = Vec::with_capacity(35);
    raw.push(role.prefix_byte());
    raw.extend_from_slice(public);
    let crc = crc16_xmodem(&raw);
    raw.extend_from_slice(&crc.to_le_bytes());
    BASE32_NOPAD.encode(&raw)
}

fn crc16_xmodem(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for byte in data {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::{encode_public, NkeyRole};
    use nkeys::{KeyPair, KeyPairType};

    #[test]
    fn encoder_matches_the_nkeys_crate_for_every_role() {
        for (role, kind) in [
            (NkeyRole::Operator, KeyPairType::Operator),
            (NkeyRole::Account, KeyPairType::Account),
            (NkeyRole::User, KeyPairType::User),
        ] {
            for seed_byte in [0u8, 7, 255] {
                let pair = KeyPair::new_from_raw(kind.clone(), [seed_byte; 32]).unwrap();
                let (_, public) = nkeys::from_public_key(&pair.public_key()).unwrap();
                assert_eq!(encode_public(role, &public), pair.public_key());
            }
        }
    }
}
