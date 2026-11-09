//! The device's WireGuard private key.
//!
//! A [`DeviceKey`] is wiped from memory when it's dropped, never printed
//! (its `Debug` output shows only the public key), and leaves this type only
//! as base64 text on its way into a key store.

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

use crate::config::value::WireGuardPublicKey;

/// A WireGuard private key belonging to this device, for one profile.
pub struct DeviceKey(StaticSecret);

/// Stored key data that isn't a WireGuard private key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the stored device key is damaged: it isn't 32 bytes of base64")]
pub struct InvalidKey;

impl DeviceKey {
    /// A new random key from the operating system's secure random number
    /// generator.
    pub fn generate() -> Self {
        Self(StaticSecret::random())
    }

    /// The public key, which is what the WireGuard server is told about.
    pub fn public_key(&self) -> WireGuardPublicKey {
        WireGuardPublicKey::from_bytes(PublicKey::from(&self.0).to_bytes())
    }

    /// The key as base64, the format `wg genkey` prints. The text is wiped
    /// when dropped.
    pub fn to_base64(&self) -> Zeroizing<String> {
        Zeroizing::new(BASE64.encode(self.0.as_bytes()))
    }

    /// Reads a key written by [`Self::to_base64`] (or `wg genkey`).
    /// Surrounding whitespace is ignored.
    pub fn from_base64(text: &str) -> Result<Self, InvalidKey> {
        let bytes = Zeroizing::new(BASE64.decode(text.trim()).map_err(|_| InvalidKey)?);
        let array: Zeroizing<[u8; 32]> =
            Zeroizing::new(bytes.as_slice().try_into().map_err(|_| InvalidKey)?);
        Ok(Self(StaticSecret::from(*array)))
    }
}

impl DeviceKey {
    /// A second copy, for when one goes to the tunnel and one to the key
    /// store. Both are wiped when dropped.
    pub(crate) fn duplicate(&self) -> Self {
        Self(self.0.clone())
    }

    /// Hands the key to the WireGuard implementation, which keeps it for the
    /// tunnel's lifetime and wipes it when the tunnel is dropped.
    pub(crate) fn into_secret(self) -> StaticSecret {
        self.0
    }
}

impl fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceKey(public: {})", self.public_key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7748 section 6.1: Alice's private and public keys.
    const RFC_PRIVATE: &str = "dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo=";
    const RFC_PUBLIC: &str = "hSDwCYkwp1R0i33ctD73Wg2/Og0mOBr066SpjqqbTmo=";

    #[test]
    fn public_key_matches_the_rfc_test_vector() {
        let key = DeviceKey::from_base64(RFC_PRIVATE).unwrap();
        assert_eq!(key.public_key().to_string(), RFC_PUBLIC);
    }

    #[test]
    fn base64_round_trips() {
        let key = DeviceKey::generate();
        let again = DeviceKey::from_base64(&key.to_base64()).unwrap();
        assert_eq!(again.public_key(), key.public_key());
        let with_newline = format!("{}\n", key.to_base64().as_str());
        assert!(DeviceKey::from_base64(&with_newline).is_ok());
    }

    #[test]
    fn generated_keys_differ() {
        assert_ne!(
            DeviceKey::generate().public_key(),
            DeviceKey::generate().public_key()
        );
    }

    #[test]
    fn damaged_keys_are_rejected() {
        for bad in ["", "not base64!", "AAAA", &format!("{RFC_PRIVATE}AAAA")] {
            assert_eq!(
                DeviceKey::from_base64(bad).unwrap_err(),
                InvalidKey,
                "{bad}"
            );
        }
    }

    #[test]
    fn debug_output_never_shows_the_private_key() {
        let key = DeviceKey::from_base64(RFC_PRIVATE).unwrap();
        let debug = format!("{key:?}");
        assert!(!debug.contains(RFC_PRIVATE), "{debug}");
        assert!(debug.contains(RFC_PUBLIC), "{debug}");
    }
}
