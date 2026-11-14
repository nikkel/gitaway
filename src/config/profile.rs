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
//! # host_key is optional: `gitaway check` pins it through the tunnel
//!
//! [hosts]                           # LAN address -> address through the tunnel
//! "192.0.2.10" = "10.0.0.1"
//!
//! [web]                             # optional: for `gitaway tunnel`
//! url = "http://192.0.2.10:3000"    # the git server's web address at home
//! # local_port = 3000               # where `tunnel` serves it; default: url's port
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
use super::value::{HostKey, InterfaceAddress, WebUrl, WireGuardPublicKey};

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
    /// The git server's web interface, which `gitaway tunnel` serves on
    /// this computer.
    pub web: Option<Web>,
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
    /// The public host key that server must prove it holds. Until one is
    /// pinned (`gitaway check` fetches it through the tunnel), gitaway can't
    /// recognise home, so it always uses the tunnel.
    pub host_key: Option<HostKey>,
}

impl Home {
    /// The SSH port, defaulting to 22.
    pub fn ssh_port(&self) -> u16 {
        self.ssh.port.unwrap_or(22)
    }
}

/// The `[web]` table: the git server's web interface, for `gitaway tunnel`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Web {
    /// Its address on the home network, as you'd open it at home.
    pub url: WebUrl,
    /// The port `gitaway tunnel` listens on, on this computer. Defaults to
    /// the url's own port.
    pub local_port: Option<u16>,
}

impl Web {
    /// The port `gitaway tunnel` listens on: `local_port`, else the url's.
    pub fn local_port(&self) -> u16 {
        self.local_port.unwrap_or_else(|| self.url.port())
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

    /// Whether `address` is one [`HostMap::route`] can return for some host
    /// and port. A host-only entry covers every port of its address, except
    /// the ports an exact `host:port` entry for the same host sends
    /// elsewhere.
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
    ///     "192.0.2.20:443" = "10.0.0.2:3000"
    ///     "192.0.2.10:2222" = "10.0.0.5:22"
    /// "#).unwrap();
    /// assert!(profile.hosts.reaches("10.0.0.1:22".parse().unwrap()));
    /// assert!(profile.hosts.reaches("10.0.0.2:3000".parse().unwrap()));
    /// // 192.0.2.10:2222 goes to 10.0.0.5:22 instead.
    /// assert!(!profile.hosts.reaches("10.0.0.1:2222".parse().unwrap()));
    /// assert!(!profile.hosts.reaches("10.0.0.2:22".parse().unwrap()));
    /// assert!(!profile.hosts.reaches("10.0.0.3:22".parse().unwrap()));
    /// ```
    pub fn reaches(&self, address: SocketAddr) -> bool {
        // A host-only entry keeps the port, so the only port that could
        // reach `address` through it is `address`'s own.
        self.0.keys().any(|key| {
            let port = key.port.unwrap_or(address.port());
            self.route(&key.host, port) == Some(address)
        })
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
        if self
            .web
            .as_ref()
            .is_some_and(|web| web.local_port == Some(0))
        {
            return Err(ParseError::Invalid(
                "web.local_port must be a port number from 1 to 65535".to_owned(),
            ));
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
    fn web_is_optional_and_its_local_port_defaults_to_the_urls() {
        assert_eq!(with_transport("").unwrap().web, None);
        let web = with_transport("[web]\nurl = \"http://192.0.2.10:3000\"")
            .unwrap()
            .web
            .unwrap();
        assert_eq!(web.local_port(), 3000);
        let web = with_transport("[web]\nurl = \"https://192.0.2.10\"\nlocal_port = 8443")
            .unwrap()
            .web
            .unwrap();
        assert_eq!(web.url.port(), 443);
        assert_eq!(web.local_port(), 8443);
    }

    #[test]
    fn invalid_web_settings_are_rejected() {
        for bad in [
            "[web]\nurl = \"ssh://192.0.2.10:2222\"",
            "[web]\nlocal_port = 3000",
            "[web]\nurl = \"http://192.0.2.10:3000\"\nlocal_port = 0",
            "[web]\nurl = \"http://192.0.2.10:3000\"\nport = 3000",
        ] {
            assert!(with_transport(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn home_may_wait_for_its_host_key() {
        let profile = with_transport("[home]\nssh = \"192.0.2.10:2222\"").unwrap();
        assert_eq!(profile.home.unwrap().host_key, None);
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

    /// The tunnel addresses a profile reaches are exactly those `route` can
    /// return: an exact-port entry hides that port of the host-only entry.
    #[test]
    fn hosts_reach_only_addresses_a_route_can_return() {
        let profile = with_transport(
            r#"
            [hosts]
            "192.0.2.10" = "10.0.0.1"
            "192.0.2.10:2222" = "10.0.0.5:22"
            "[2001:db8::10]" = "[fd00::1]"
            "#,
        )
        .unwrap();
        let reaches = |address: &str| profile.hosts.reaches(address.parse().unwrap());

        assert!(reaches("10.0.0.1:443"));
        assert!(reaches("10.0.0.5:22"));
        assert!(reaches("[fd00::1]:22"));
        assert!(
            !reaches("10.0.0.1:2222"),
            "the exact-port entry overrides it"
        );
        assert!(!reaches("10.0.0.5:2222"));
        assert!(!reaches("10.0.0.9:443"));

        // Another host-only entry for the same address still reaches that port.
        let profile = with_transport(
            r#"
            [hosts]
            "192.0.2.10" = "10.0.0.1"
            "192.0.2.10:2222" = "10.0.0.5:22"
            "nas.example.com" = "10.0.0.1"
            "#,
        )
        .unwrap();
        assert!(profile.hosts.reaches("10.0.0.1:2222".parse().unwrap()));
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
