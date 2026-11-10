//! A WireGuard preshared key: an optional second secret a server and one
//! device share, mixed into their handshake on top of the key pairs.
//!
//! Servers such as wg-easy and PiVPN add one to every device's config by
//! default (`PresharedKey = …` under `[Peer]`). Both sides must have the
//! same one, or the handshake fails. Like the device key, it's wiped from
//! memory when dropped, never printed, and kept only in the key store.

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use zeroize::Zeroizing;

/// A preshared key: 32 secret bytes.
pub struct PresharedKey(Zeroizing<[u8; 32]>);

/// Text that isn't a preshared key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a preshared key is 32 bytes of base64, as `wg genpsk` prints")]
pub struct InvalidPresharedKey;

impl PresharedKey {
    /// Reads a key as `wg genpsk` prints it. Surrounding whitespace is
    /// ignored.
    pub fn from_base64(text: &str) -> Result<Self, InvalidPresharedKey> {
        let bytes = Zeroizing::new(
            BASE64
                .decode(text.trim())
                .map_err(|_| InvalidPresharedKey)?,
        );
        let array: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| InvalidPresharedKey)?;
        Ok(Self(Zeroizing::new(array)))
    }

    /// The key as base64, for the key store. The text is wiped when dropped.
    pub fn to_base64(&self) -> Zeroizing<String> {
        Zeroizing::new(BASE64.encode(self.0.as_slice()))
    }

    /// The raw bytes, for the WireGuard implementation.
    pub(crate) fn to_bytes(&self) -> [u8; 32] {
        *self.0
    }

    /// A second copy, for when one goes to the tunnel and one to the key
    /// store.
    pub(crate) fn duplicate(&self) -> Self {
        Self(self.0.clone())
    }
}

impl fmt::Debug for PresharedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PresharedKey(…)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An example preshared key (bytes 1 to 32), not a secret.
    const EXAMPLE: &str = "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA=";

    #[test]
    fn base64_round_trips_and_whitespace_is_ignored() {
        let key = PresharedKey::from_base64(&format!("  {EXAMPLE}\n")).unwrap();
        assert_eq!(key.to_base64().as_str(), EXAMPLE);
        assert_eq!(key.to_bytes()[..3], [1, 2, 3]);
    }

    #[test]
    fn damaged_keys_are_rejected() {
        for bad in ["", "not base64!", "AAAA", &format!("{EXAMPLE}AAAA")] {
            assert!(PresharedKey::from_base64(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn debug_output_never_shows_the_key() {
        let key = PresharedKey::from_base64(EXAMPLE).unwrap();
        assert_eq!(format!("{key:?}"), "PresharedKey(…)");
    }
}
