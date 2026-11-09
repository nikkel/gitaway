//! Network addresses written as `host` or `host:port`.
//!
//! The same syntax appears in remote URLs, in a profile's `[home].ssh` and
//! `endpoint`, and in its `[hosts]` table, so it's parsed in exactly one place.
//! Hosts are lower-cased so `Example.COM` and `example.com` compare equal.
//! IPv6 addresses are written in brackets (`[2001:db8::1]:22`) and stored
//! without them.

use std::fmt;
use std::net::Ipv6Addr;
use std::str::FromStr;

/// A host with an optional port.
///
/// ```
/// use gitaway::address::HostPort;
///
/// let address: HostPort = "192.0.2.10:2222".parse().unwrap();
/// assert_eq!(address.host, "192.0.2.10");
/// assert_eq!(address.port, Some(2222));
/// assert_eq!(address.to_string(), "192.0.2.10:2222");
///
/// let v6: HostPort = "[2001:DB8::1]".parse().unwrap();
/// assert_eq!(v6.host, "2001:db8::1");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Deserialize)]
#[serde(try_from = "String")]
pub struct HostPort {
    /// A DNS name, IPv4 address or IPv6 address (without brackets), lower-cased.
    pub host: String,
    /// The port, if one was given.
    pub port: Option<u16>,
}

/// Why a `host[:port]` string was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{input}` is not a valid host or host:port")]
pub struct InvalidAddress {
    input: String,
}

impl FromStr for HostPort {
    type Err = InvalidAddress;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse(s).ok_or_else(|| InvalidAddress {
            input: s.to_owned(),
        })
    }
}

impl TryFrom<String> for HostPort {
    type Error = InvalidAddress;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl fmt::Display for HostPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let is_v6 = self.host.contains(':');
        match (is_v6, self.port) {
            (true, Some(port)) => write!(f, "[{}]:{port}", self.host),
            (true, None) => write!(f, "[{}]", self.host),
            (false, Some(port)) => write!(f, "{}:{port}", self.host),
            (false, None) => f.write_str(&self.host),
        }
    }
}

fn parse(s: &str) -> Option<HostPort> {
    if let Some(bracketed) = s.strip_prefix('[') {
        let (inner, after) = bracketed.split_once(']')?;
        let host = inner.parse::<Ipv6Addr>().ok()?.to_string();
        let port = match after {
            "" => None,
            _ => Some(parse_port(after.strip_prefix(':')?)?),
        };
        return Some(HostPort { host, port });
    }
    let (host, port) = match s.split_once(':') {
        Some((host, port)) => (host, Some(parse_port(port)?)),
        None => (s, None),
    };
    Some(HostPort {
        host: validate_host(host)?,
        port,
    })
}

fn parse_port(port: &str) -> Option<u16> {
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    port.parse().ok().filter(|&p| p != 0)
}

/// Accepts DNS names and IPv4 addresses. Rejects anything with characters a
/// hostname can't contain, so strings like `key=value` or `-oOption` are
/// never mistaken for a host.
fn validate_host(host: &str) -> Option<String> {
    let valid = !host.is_empty()
        && !host.starts_with('-')
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    valid.then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hp(host: &str, port: Option<u16>) -> HostPort {
        HostPort {
            host: host.to_owned(),
            port,
        }
    }

    #[test]
    fn valid_addresses() {
        let cases = [
            ("example.com", hp("example.com", None)),
            ("Example.COM:22", hp("example.com", Some(22))),
            ("192.0.2.10:65535", hp("192.0.2.10", Some(65535))),
            ("nas", hp("nas", None)),
            ("[2001:db8::1]", hp("2001:db8::1", None)),
            ("[2001:DB8:0::1]:2222", hp("2001:db8::1", Some(2222))),
        ];
        for (input, expected) in cases {
            assert_eq!(input.parse::<HostPort>(), Ok(expected), "{input}");
        }
    }

    #[test]
    fn invalid_addresses() {
        for input in [
            "",
            ":22",
            "example.com:",
            "example.com:0",
            "example.com:65536",
            "example.com:22x",
            "example.com:+22",
            "-oProxyCommand",
            "key=value",
            "a b",
            "2001:db8::1",
            "[2001:db8::1",
            "[not-v6]:22",
            "[2001:db8::1]22",
        ] {
            assert!(input.parse::<HostPort>().is_err(), "{input}");
        }
    }

    #[test]
    fn display_round_trips() {
        for input in [
            "example.com",
            "example.com:22",
            "[2001:db8::1]",
            "[2001:db8::1]:22",
        ] {
            let parsed: HostPort = input.parse().unwrap();
            assert_eq!(parsed.to_string(), input);
            assert_eq!(parsed.to_string().parse::<HostPort>(), Ok(parsed));
        }
    }
}
