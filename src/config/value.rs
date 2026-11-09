//! Validated value types used in profiles.
//!
//! Each type checks its format when a profile is loaded, so a typo is
//! reported straight away with the field it's in, rather than as a confusing
//! failure when connecting. None of these are secrets: public keys are safe
//! to print.

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use russh::keys::{Algorithm, HashAlg, PublicKey};

/// A value in a profile didn't have the expected format.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{value}` is not a valid {expected}")]
pub struct InvalidValue {
    value: String,
    expected: &'static str,
}

impl InvalidValue {
    fn new(value: &str, expected: &'static str) -> Self {
        Self {
            value: value.to_owned(),
            expected,
        }
    }
}

/// An SSH server's public host key, in OpenSSH's one-line format:
/// `<type> <base64> [comment]`. That's what `ssh-keyscan` prints after the
/// host name, and what a server's `/etc/ssh/ssh_host_*_key.pub` files hold.
///
/// The whole key is pinned, not just its fingerprint, because the key type
/// matters: a server with several host keys presents whichever type the
/// client asks for, so the home check asks for exactly this one.
///
/// ```
/// use gitaway::config::value::HostKey;
///
/// let key: HostKey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDtqJ7zOtqQtYqOo0CpvDXNlMhV3HeJDpjrASKGLWdop"
///     .parse()
///     .unwrap();
/// assert_eq!(key.fingerprint(), "SHA256:tAXFyTXI8xtDaujAEcwJslAYc9/6FKcUkd2Lw0xDhPo");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(try_from = "String")]
pub struct HostKey(PublicKey);

impl HostKey {
    /// The key's type, such as Ed25519.
    pub fn algorithm(&self) -> Algorithm {
        self.0.algorithm()
    }

    /// The SHA-256 fingerprint, formatted as `ssh-keygen -l` prints it, for
    /// people to compare by eye.
    pub fn fingerprint(&self) -> String {
        self.0.fingerprint(HashAlg::Sha256).to_string()
    }

    /// Whether `key` is this key. Comments are ignored.
    pub fn matches(&self, key: &PublicKey) -> bool {
        self.0.key_data() == key.key_data()
    }
}

impl From<PublicKey> for HostKey {
    fn from(key: PublicKey) -> Self {
        Self(key)
    }
}

impl FromStr for HostKey {
    type Err = InvalidValue;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || {
            InvalidValue::new(
                s,
                "SSH host key (`ssh-ed25519 AAAA...` as printed by ssh-keyscan, without the host name)",
            )
        };
        let key = PublicKey::from_openssh(s.trim()).map_err(|_| invalid())?;
        match key.algorithm() {
            Algorithm::Ed25519 | Algorithm::Ecdsa { .. } | Algorithm::Rsa { .. } => Ok(Self(key)),
            _ => Err(invalid()),
        }
    }
}

impl TryFrom<String> for HostKey {
    type Error = InvalidValue;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl fmt::Display for HostKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.to_openssh().map_err(|_| fmt::Error)?)
    }
}

/// A WireGuard public key: 32 bytes in base64, as printed by `wg pubkey`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(try_from = "String")]
pub struct WireGuardPublicKey(String);

impl WireGuardPublicKey {
    /// The key for 32 raw bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(BASE64.encode(bytes))
    }

    /// The key's 32 raw bytes.
    pub fn to_bytes(&self) -> [u8; 32] {
        decode_32_bytes(&self.0).expect("validated when created")
    }
}

impl FromStr for WireGuardPublicKey {
    type Err = InvalidValue;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if decode_32_bytes(s).is_some() {
            Ok(Self(s.to_owned()))
        } else {
            Err(InvalidValue::new(
                s,
                "WireGuard public key (44 base64 characters)",
            ))
        }
    }
}

/// A tunnel address with its prefix length, such as `10.0.0.2/32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(try_from = "String")]
pub struct InterfaceAddress {
    /// The address.
    pub ip: IpAddr,
    /// The prefix length: up to 32 for IPv4, 128 for IPv6.
    pub prefix: u8,
}

impl FromStr for InterfaceAddress {
    type Err = InvalidValue;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidValue::new(s, "address with prefix (such as 10.0.0.2/32)");
        let (ip, prefix) = s.split_once('/').ok_or_else(invalid)?;
        let ip: IpAddr = ip.parse().map_err(|_| invalid())?;
        let prefix: u8 = prefix
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| prefix.parse().ok())
            .flatten()
            .ok_or_else(invalid)?;
        let max = if ip.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(invalid());
        }
        Ok(Self { ip, prefix })
    }
}

impl fmt::Display for InterfaceAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.ip, self.prefix)
    }
}

/// Implements `TryFrom<String>` (which serde uses) and `Display` for the
/// string-backed types, so each isn't written out three times.
macro_rules! string_backed {
    ($($ty:ident),*) => {$(
        impl TryFrom<String> for $ty {
            type Error = InvalidValue;
            fn try_from(s: String) -> Result<Self, Self::Error> {
                s.parse()
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    )*};
}
string_backed!(WireGuardPublicKey);

impl TryFrom<String> for InterfaceAddress {
    type Error = InvalidValue;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

/// Decodes standard, padded base64 holding exactly 32 bytes. The decoder
/// rejects non-canonical encodings (spare bits that aren't zero), so a string
/// that's the right length but can't have come from a real key is refused.
fn decode_32_bytes(s: &str) -> Option<[u8; 32]> {
    BASE64.decode(s).ok()?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Ed25519 public key for an all-zero seed: a standard test vector.
    pub(crate) const HOST_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDtqJ7zOtqQtYqOo0CpvDXNlMhV3HeJDpjrASKGLWdop";
    // 32 zero bytes, base64-encoded.
    const PUBLIC_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    #[test]
    fn host_keys() {
        let key: HostKey = HOST_KEY.parse().unwrap();
        assert_eq!(key.algorithm(), Algorithm::Ed25519);
        assert_eq!(key.to_string(), HOST_KEY);

        let with_comment: HostKey = format!("  {HOST_KEY} root@nas\n").parse().unwrap();
        assert!(
            with_comment.matches(&key.0),
            "comment and whitespace are ignored"
        );

        for bad in [
            "",
            "SHA256:tAXFyTXI8xtDaujAEcwJslAYc9/6FKcUkd2Lw0xDhPo",
            &format!("192.0.2.10 {HOST_KEY}"),
            "ssh-ed25519",
            "ssh-ed25519 not-base64!",
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDtqJ7zOtqQtYqOo0CpvDXNlMhV3HeJDpjrASKGLWdo",
            "ssh-rsa AAAAC3NzaC1lZDI1NTE5AAAAIDtqJ7zOtqQtYqOo0CpvDXNlMhV3HeJDpjrASKGLWdop",
        ] {
            assert!(bad.parse::<HostKey>().is_err(), "{bad}");
        }
    }

    #[test]
    fn wireguard_public_keys() {
        assert!(PUBLIC_KEY.parse::<WireGuardPublicKey>().is_ok());
        for bad in [
            "",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB=",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA_=",
        ] {
            assert!(bad.parse::<WireGuardPublicKey>().is_err(), "{bad}");
        }
    }

    #[test]
    fn wireguard_public_keys_round_trip_through_bytes() {
        let bytes: [u8; 32] = std::array::from_fn(|i| i as u8 * 7);
        let key = WireGuardPublicKey::from_bytes(bytes);
        assert_eq!(key.to_bytes(), bytes);
        assert_eq!(key.to_string().parse::<WireGuardPublicKey>().unwrap(), key);
    }

    #[test]
    fn interface_addresses() {
        let v4: InterfaceAddress = "10.0.0.2/32".parse().unwrap();
        assert_eq!(v4.to_string(), "10.0.0.2/32");
        assert!("2001:db8::2/128".parse::<InterfaceAddress>().is_ok());
        for bad in [
            "10.0.0.2",
            "10.0.0.2/33",
            "2001:db8::2/129",
            "10.0.0.2/",
            "10.0.0.2/+8",
            "10.0.0/24",
            "vpn.example.net/32",
        ] {
            assert!(bad.parse::<InterfaceAddress>().is_err(), "{bad}");
        }
    }
}
