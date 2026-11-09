//! Validated value types used in profiles.
//!
//! Each type checks its format when a profile is loaded, so a typo is
//! reported straight away with the field it's in, rather than as a confusing
//! failure when connecting. None of these are secrets: fingerprints and
//! public keys are safe to print.

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

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

/// An SSH host-key fingerprint in OpenSSH's format, `SHA256:<base64>`, as
/// printed by `ssh-keygen -lf` or `ssh-keyscan host | ssh-keygen -lf -`.
///
/// ```
/// use gitaway::config::value::Fingerprint;
///
/// let fp: Fingerprint = "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU".parse().unwrap();
/// assert!("MD5:aa:bb".parse::<Fingerprint>().is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(try_from = "String")]
pub struct Fingerprint(String);

impl FromStr for Fingerprint {
    type Err = InvalidValue;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.strip_prefix("SHA256:")
            .filter(|b64| is_base64_of_32_bytes(b64, false))
            .map(|_| Self(s.to_owned()))
            .ok_or_else(|| InvalidValue::new(s, "SHA256 host-key fingerprint (SHA256:...)"))
    }
}

/// A WireGuard public key: 32 bytes in base64, as printed by `wg pubkey`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(try_from = "String")]
pub struct WireGuardPublicKey(String);

impl FromStr for WireGuardPublicKey {
    type Err = InvalidValue;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if is_base64_of_32_bytes(s, true) {
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
string_backed!(Fingerprint, WireGuardPublicKey);

impl TryFrom<String> for InterfaceAddress {
    type Error = InvalidValue;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

/// Whether `s` is standard base64 encoding exactly 32 bytes: 43 characters,
/// plus `=` if `padded`.
///
/// 32 bytes is 256 bits, but 43 base64 characters hold 258. The last
/// character's two spare bits must be zero, which only these characters
/// satisfy. Checking that rejects strings that are the right length but can't
/// have come from a real key.
fn is_base64_of_32_bytes(s: &str, padded: bool) -> bool {
    const LAST_CHARS: &[u8] = b"AEIMQUYcgkosw048";
    let body = if padded { s.strip_suffix('=') } else { Some(s) };
    let Some(body) = body else { return false };
    let bytes = body.as_bytes();
    bytes.len() == 43
        && bytes
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
        && LAST_CHARS.contains(&bytes[42])
}

#[cfg(test)]
mod tests {
    use super::*;

    // SHA-256 of the empty string, so a real digest with no meaning.
    const FINGERPRINT: &str = "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU";
    // 32 zero bytes, base64-encoded.
    const PUBLIC_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    #[test]
    fn fingerprints() {
        assert!(FINGERPRINT.parse::<Fingerprint>().is_ok());
        for bad in [
            "",
            "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU",
            "sha256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU",
            "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=",
            "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuF",
            "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFV",
            "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSu-U",
            "MD5:16:27:ac:a5:76:28:2d:36:63:1b:56:4d:eb:df:a6:48",
        ] {
            assert!(bad.parse::<Fingerprint>().is_err(), "{bad}");
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
