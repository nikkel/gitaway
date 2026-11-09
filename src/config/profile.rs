//! The profile file format and its validation.
//!
//! A profile is one place you connect to. Example (documentation values):
//!
//! ```toml
//! [transport.wireguard]
//! endpoint = "vpn.example.net:51820"
//! server_public_key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
//! address = "10.0.0.2/32"          # optional: often assigned by the server later
//! # mtu = 1420                     # optional
//!
//! [home]                            # how to tell you're on this LAN
//! ssh = "192.0.2.10:2222"
//! host_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDtqJ7zOtqQtYqOo0CpvDXNlMhV3HeJDpjrASKGLWdop"
//!
//! [hosts]                           # LAN address -> address through the tunnel
//! "192.0.2.10" = "10.0.0.1"
//! ```
//!
//! Unknown fields are errors, so a typo is reported instead of silently
//! ignored. Exactly one transport table is allowed, which serde enforces by
//! reading `transport` as an enum.
//!
//! # Extending
//!
//! A new transport is a new variant of [`Transport`] with its own settings
//! struct. Because the enum is `#[non_exhaustive]`, adding a variant doesn't
//! break code that uses the library.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};

use crate::address::HostPort;
use crate::git::remote_url::RemoteUrl;

use super::name::ProfileName;
use super::value::{HostKey, InterfaceAddress, WireGuardPublicKey};

/// The smallest MTU accepted. Anything lower can't carry a useful TCP segment.
pub const MIN_MTU: u16 = 576;

/// One place you connect to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// How to reach the place when you're away.
    pub transport: Transport,
    /// How to tell you're there. Without it, gitaway always assumes you're
    /// away.
    pub home: Option<Home>,
    /// Which remote hosts this profile handles, and their addresses through
    /// the tunnel.
    #[serde(default)]
    pub hosts: HostMap,
}

/// The VPN used to reach a profile from elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Transport {
    /// A WireGuard tunnel run inside the gitaway process.
    #[serde(rename = "wireguard")]
    WireGuard(WireGuard),
}

/// Settings for a WireGuard tunnel. The device's private key isn't here: it
/// lives in the OS keychain.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireGuard {
    /// The server's public `host:port`, usually a DDNS name.
    pub endpoint: HostPort,
    /// The server's public key.
    pub server_public_key: WireGuardPublicKey,
    /// This device's address inside the tunnel, once the server has assigned it.
    pub address: Option<InterfaceAddress>,
    /// The tunnel MTU, if the default doesn't suit the network.
    pub mtu: Option<u16>,
}

/// How to verify you're on the profile's home network.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Home {
    /// The SSH server to check. The port defaults to 22.
    pub ssh: HostPort,
    /// The public host key that server must prove it holds.
    pub host_key: HostKey,
}

impl Home {
    /// The SSH port, defaulting to 22.
    pub fn ssh_port(&self) -> u16 {
        self.ssh.port.unwrap_or(22)
    }
}

/// The `[hosts]` table: LAN address to tunnel address.
///
/// Entries are either host to host (the port is kept), or `host:port` to
/// `host:port`. Mixing the two in one entry is an error, as are two keys that
/// mean the same thing (`Example.com` and `example.com`). The tunnel side
/// must be an IP address: names can't be looked up through the tunnel.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(try_from = "BTreeMap<String, String>")]
pub struct HostMap(BTreeMap<HostPort, HostPort>);

impl HostMap {
    /// Whether a remote is one this profile handles: its host matches a key,
    /// and the key's port (if it has one) matches the remote's port.
    ///
    /// Remotes this returns `false` for are run with plain git, with no home
    /// check.
    pub fn handles(&self, remote: &RemoteUrl) -> bool {
        self.route(&remote.host, remote.effective_port()).is_some()
    }

    /// Where `host:port` is reached through the tunnel, or `None` if this
    /// profile doesn't handle it. A `host:port` entry wins over a host-only
    /// entry for the same host. `host` is compared without regard to case,
    /// and may be an IPv6 address with or without brackets.
    ///
    /// ```
    /// use gitaway::config::profile::Profile;
    ///
    /// let profile = Profile::from_toml(r#"
    ///     [transport.wireguard]
    ///     endpoint = "vpn.example.net:51820"
    ///     server_public_key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
    ///     [hosts]
    ///     "192.0.2.10" = "10.0.0.1"
    /// "#).unwrap();
    /// assert_eq!(profile.hosts.route("192.0.2.10", 2222), Some("10.0.0.1:2222".parse().unwrap()));
    /// assert_eq!(profile.hosts.route("github.com", 22), None);
    /// ```
    pub fn route(&self, host: &str, port: u16) -> Option<SocketAddr> {
        let host = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host)
            .to_ascii_lowercase();
        let exact = self
            .0
            .iter()
            .find(|(key, _)| key.host == host && key.port == Some(port));
        let host_only = || {
            self.0
                .iter()
                .find(|(key, _)| key.host == host && key.port.is_none())
        };
        let (_, tunnel) = exact.or_else(host_only)?;
        let ip: IpAddr = tunnel
            .host
            .parse()
            .expect("validated: the tunnel side is an IP");
        Some(SocketAddr::new(ip, tunnel.port.unwrap_or(port)))
    }

    /// Whether there are no entries.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl TryFrom<BTreeMap<String, String>> for HostMap {
    type Error = String;

    fn try_from(raw: BTreeMap<String, String>) -> Result<Self, Self::Error> {
        let mut map = BTreeMap::new();
        for (from, to) in raw {
            let from_addr: HostPort = from.parse().map_err(|e| format!("[hosts] key: {e}"))?;
            let to_addr: HostPort = to.parse().map_err(|e| format!("[hosts] `{from}`: {e}"))?;
            if to_addr.host.parse::<IpAddr>().is_err() {
                return Err(format!(
                    "[hosts] `{from}` = `{to}`: the tunnel side must be an IP address, such as 10.0.0.1"
                ));
            }
            if from_addr.port.is_some() != to_addr.port.is_some() {
                return Err(format!(
                    "[hosts] `{from}` = `{to}`: use host = host, or host:port = host:port"
                ));
            }
            if map.insert(from_addr, to_addr).is_some() {
                return Err(format!("[hosts] `{from}` is listed more than once"));
            }
        }
        Ok(Self(map))
    }
}

/// The settings that aren't per profile, from `config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalConfig {
    /// The profile to use when nothing more specific applies.
    pub default_profile: Option<ProfileName>,
}

/// A profile or `config.toml` couldn't be loaded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseError {
    /// Not valid TOML, an unknown field, or a value in the wrong format. The
    /// message includes the line and column.
    #[error(transparent)]
    Toml(#[from] toml::de::Error),
    /// Valid on its own, but not in combination with other settings.
    #[error("{0}")]
    Invalid(String),
}

impl Profile {
    /// Parses and validates a profile file's contents.
    ///
    /// ```
    /// use gitaway::config::profile::Profile;
    ///
    /// let profile = Profile::from_toml(r#"
    ///     [transport.wireguard]
    ///     endpoint = "vpn.example.net:51820"
    ///     server_public_key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
    /// "#).unwrap();
    /// assert!(profile.home.is_none());
    /// ```
    pub fn from_toml(text: &str) -> Result<Self, ParseError> {
        let profile: Self = toml::from_str(text)?;
        profile.validate()?;
        Ok(profile)
    }

    fn validate(&self) -> Result<(), ParseError> {
        match &self.transport {
            Transport::WireGuard(wg) => {
                if wg.endpoint.port.is_none() {
                    return Err(ParseError::Invalid(format!(
                        "transport.wireguard.endpoint `{}` needs a port, such as {}:51820",
                        wg.endpoint, wg.endpoint
                    )));
                }
                if wg.mtu.is_some_and(|mtu| mtu < MIN_MTU) {
                    return Err(ParseError::Invalid(format!(
                        "transport.wireguard.mtu must be at least {MIN_MTU}"
                    )));
                }
            }
        }
        Ok(())
    }
}

impl GlobalConfig {
    /// Parses `config.toml`'s contents.
    pub fn from_toml(text: &str) -> Result<Self, ParseError> {
        Ok(toml::from_str(text)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::remote_url;

    const TRANSPORT: &str = r#"
        [transport.wireguard]
        endpoint = "vpn.example.net:51820"
        server_public_key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
    "#;

    fn with_transport(rest: &str) -> Result<Profile, ParseError> {
        Profile::from_toml(&format!("{TRANSPORT}\n{rest}"))
    }

    #[test]
    fn full_profile_parses() {
        let profile = Profile::from_toml(
            r#"
            [transport.wireguard]
            endpoint = "vpn.example.net:51820"
            server_public_key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
            address = "10.0.0.2/32"
            mtu = 1420

            [home]
            ssh = "192.0.2.10:2222"
            host_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDtqJ7zOtqQtYqOo0CpvDXNlMhV3HeJDpjrASKGLWdop"

            [hosts]
            "192.0.2.10" = "10.0.0.1"
            "192.0.2.11:3000" = "10.0.0.1:3000"
            "#,
        )
        .unwrap();

        let Transport::WireGuard(wg) = &profile.transport;
        assert_eq!(wg.endpoint.to_string(), "vpn.example.net:51820");
        assert_eq!(wg.mtu, Some(1420));
        assert_eq!(profile.home.as_ref().unwrap().ssh_port(), 2222);
        assert!(!profile.hosts.is_empty());
    }

    #[test]
    fn home_ssh_port_defaults_to_22() {
        let profile = with_transport(
            r#"
            [home]
            ssh = "192.0.2.10"
            host_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDtqJ7zOtqQtYqOo0CpvDXNlMhV3HeJDpjrASKGLWdop"
            "#,
        )
        .unwrap();
        assert_eq!(profile.home.unwrap().ssh_port(), 22);
    }

    #[test]
    fn invalid_profiles_are_rejected() {
        let cases = [
            (
                "missing transport",
                "[hosts]\n\"192.0.2.10\" = \"10.0.0.1\"",
            ),
            (
                "unknown transport",
                "[transport.carrier-pigeon]\nendpoint = \"x:1\"",
            ),
            (
                "unknown top-level field",
                &format!("{TRANSPORT}\nsecret = 1"),
            ),
            (
                "two transports",
                &format!("{TRANSPORT}\n[transport.openvpn]\nconfig = \"x\""),
            ),
            (
                "endpoint without port",
                "[transport.wireguard]\nendpoint = \"vpn.example.net\"\nserver_public_key = \"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"",
            ),
            (
                "bad public key",
                "[transport.wireguard]\nendpoint = \"vpn.example.net:1\"\nserver_public_key = \"nope\"",
            ),
            (
                "private key in profile",
                &format!(
                    "{TRANSPORT}private_key = \"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\""
                ),
            ),
            ("tiny mtu", &format!("{TRANSPORT}mtu = 100")),
            (
                "bad host key",
                &format!("{TRANSPORT}\n[home]\nssh = \"192.0.2.10\"\nhost_key = \"abc\""),
            ),
            (
                "home without host_key",
                &format!("{TRANSPORT}\n[home]\nssh = \"192.0.2.10\""),
            ),
            (
                "mixed host and host:port",
                &format!("{TRANSPORT}\n[hosts]\n\"192.0.2.10:22\" = \"10.0.0.1\""),
            ),
            (
                "duplicate host after normalising",
                &format!(
                    "{TRANSPORT}\n[hosts]\n\"Example.com\" = \"10.0.0.1\"\n\"example.com\" = \"10.0.0.2\""
                ),
            ),
            (
                "bad host",
                &format!("{TRANSPORT}\n[hosts]\n\"-oProxyCommand=x\" = \"10.0.0.1\""),
            ),
            (
                "tunnel side is a name",
                &format!("{TRANSPORT}\n[hosts]\n\"192.0.2.10\" = \"nas.example.net\""),
            ),
        ];
        for (what, text) in cases {
            assert!(Profile::from_toml(text).is_err(), "{what} was accepted");
        }
    }

    #[test]
    fn hosts_handle_matching_remotes_only() {
        let profile = with_transport(
            r#"
            [hosts]
            "192.0.2.10" = "10.0.0.1"
            "example.com:2222" = "10.0.0.2:2222"
            "#,
        )
        .unwrap();
        let handles = |url: &str| profile.hosts.handles(&remote_url::parse(url).unwrap());

        assert!(handles("ssh://git@192.0.2.10:2222/alice/app.git"));
        assert!(handles("http://192.0.2.10:3000/alice/app.git"));
        assert!(handles("ssh://git@Example.com:2222/alice/app.git"));
        assert!(
            !handles("git@example.com:alice/app.git"),
            "port 22 isn't 2222"
        );
        assert!(!handles("git@github.com:alice/app.git"));
        assert!(!handles("ssh://git@192.0.2.100/app.git"));
    }

    #[test]
    fn routes_prefer_an_exact_port_and_keep_the_port_otherwise() {
        let profile = with_transport(
            r#"
            [hosts]
            "192.0.2.10" = "10.0.0.1"
            "192.0.2.10:2222" = "10.0.0.5:22"
            "git.example.com" = "10.0.0.2"
            "[2001:db8::10]" = "[fd00::1]"
            "#,
        )
        .unwrap();
        let route = |host: &str, port: u16| profile.hosts.route(host, port).map(|a| a.to_string());

        assert_eq!(route("192.0.2.10", 443).as_deref(), Some("10.0.0.1:443"));
        assert_eq!(route("192.0.2.10", 2222).as_deref(), Some("10.0.0.5:22"));
        assert_eq!(route("GIT.Example.com", 22).as_deref(), Some("10.0.0.2:22"));
        assert_eq!(route("[2001:db8::10]", 22).as_deref(), Some("[fd00::1]:22"));
        assert_eq!(route("2001:db8::10", 22).as_deref(), Some("[fd00::1]:22"));
        assert_eq!(route("github.com", 22), None);
        assert_eq!(route("192.0.2.100", 22), None);
    }

    #[test]
    fn global_config() {
        let config = GlobalConfig::from_toml("default_profile = \"home\"").unwrap();
        assert_eq!(config.default_profile.unwrap().as_str(), "home");
        assert_eq!(
            GlobalConfig::from_toml("").unwrap(),
            GlobalConfig::default()
        );
        assert!(GlobalConfig::from_toml("default_profile = \"../x\"").is_err());
        assert!(GlobalConfig::from_toml("unknown = 1").is_err());
    }
}
