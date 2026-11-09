//! Reading the client config a VPN server gives a device, so `gitaway setup`
//! can build a profile from it.
//!
//! Every VPN server hands a new device a config file: WireGuard servers and
//! apps a `.conf`, OpenVPN servers an `.ovpn`. Reading it fills in the
//! profile's `[transport]` settings and the device's secret, so the user
//! never copies those values by hand.
//!
//! ```text
//!  text ──▶ each format: "is this yours?" ──▶ the first that says yes reads it
//!            ├─ WireGuard  (wireguard/client_config.rs)
//!            └─ … a new transport adds its format to FORMATS
//!                         │
//!                         ▼
//!          ClientConfig: transport settings, device key, routes
//! ```
//!
//! A config doesn't say where the git server is inside the tunnel, but its
//! routes narrow it down: [`ClientConfig::likely_addresses`] lists the
//! addresses worth trying, best first, and `gitaway setup` tries them.
//!
//! # Extending
//!
//! A new transport's format is a type implementing [`ConfigFormat`], in the
//! transport's own folder, added to [`FORMATS`]. See
//! `docs/adding-a-transport.md`.

use std::net::{IpAddr, Ipv4Addr};

use ipnetwork::IpNetwork;

use crate::config::profile::Transport;
use crate::keys::device_key::DeviceKey;

use super::wireguard::client_config::WireGuardConfig;

/// Every config format gitaway can read, tried in order.
pub const FORMATS: &[&dyn ConfigFormat] = &[&WireGuardConfig];

/// One VPN's client config format.
pub trait ConfigFormat: Sync {
    /// The VPN's name, as users know it, such as `WireGuard`.
    fn name(&self) -> &'static str;

    /// Whether `text` looks like this format at all. Only a format that
    /// recognises the text reads it, so its errors are the ones shown.
    fn recognises(&self, text: &str) -> bool;

    /// Reads the config. Errors must never include the text itself, which
    /// holds the device's secret.
    fn read(&self, text: &str) -> Result<ClientConfig, String>;
}

/// What a client config says about reaching home.
#[derive(Debug)]
pub struct ClientConfig {
    /// The profile's `[transport]` settings.
    pub transport: Transport,
    /// The device's private key, if the config holds one. A config made for
    /// a key the user generated elsewhere doesn't.
    pub key: Option<DeviceKey>,
    /// The addresses the config sends through the tunnel.
    pub routes: Vec<IpNetwork>,
}

/// The text isn't a config gitaway can use.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ImportError {
    /// No format recognised it.
    #[error("this isn't a VPN client config gitaway can read. It reads: {known}")]
    Unrecognised {
        /// The formats gitaway knows, for the message.
        known: String,
    },
    /// A format recognised it, but something in it is wrong or unsupported.
    #[error("this {format} config can't be used: {reason}")]
    Invalid {
        /// The format's name.
        format: &'static str,
        /// What's wrong.
        reason: String,
    },
}

/// Reads a client config in any known format.
pub fn read(text: &str) -> Result<ClientConfig, ImportError> {
    let Some(format) = FORMATS.iter().find(|format| format.recognises(text)) else {
        let known: Vec<_> = FORMATS
            .iter()
            .map(|format| format!("{} configs", format.name()))
            .collect();
        return Err(ImportError::Unrecognised {
            known: known.join(", "),
        });
    };
    format.read(text).map_err(|reason| ImportError::Invalid {
        format: format.name(),
        reason,
    })
}

impl ClientConfig {
    /// Where a server the user reaches at `lan_host` at home is most likely
    /// to be inside the tunnel, best first:
    ///
    /// 1. A single address the config routes (`AllowedIPs = 10.0.0.1/32`):
    ///    a config that sends only one address through the tunnel does so
    ///    because that's the server it's for.
    /// 2. `lan_host` itself, if it's an IP address the config routes: the
    ///    VPN server forwards to the home network, so the address works
    ///    unchanged.
    /// 3. The first address of this device's /24 (`10.0.0.1` for
    ///    `10.0.0.2`), if routed: where VPN servers usually put themselves,
    ///    for a git server running on the VPN server's machine.
    ///
    /// ```
    /// use gitaway::transport::client_config;
    ///
    /// let config = client_config::read("
    ///     [Interface]
    ///     Address = 10.0.0.2/32
    ///     [Peer]
    ///     PublicKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=
    ///     Endpoint = vpn.example.net:51820
    ///     AllowedIPs = 0.0.0.0/0
    /// ").unwrap();
    /// let likely: Vec<String> = config.likely_addresses("192.0.2.10")
    ///     .iter().map(|ip| ip.to_string()).collect();
    /// assert_eq!(likely, ["192.0.2.10", "10.0.0.1"]);
    /// ```
    pub fn likely_addresses(&self, lan_host: &str) -> Vec<IpAddr> {
        let routed = |ip: IpAddr| self.routes.iter().any(|route| route.contains(ip));
        let device = self.device_address();
        let single_hosts = self
            .routes
            .iter()
            .filter(|route| route.prefix() == max_prefix(route.ip()))
            .map(IpNetwork::ip);
        let lan = lan_host.parse::<IpAddr>().ok().filter(|&ip| routed(ip));
        let first_of_subnet = match device {
            Some(IpAddr::V4(ip)) => {
                let first = IpAddr::V4(Ipv4Addr::from(u32::from(ip) & 0xFFFF_FF00 | 1));
                Some(first).filter(|&first| routed(first))
            }
            _ => None,
        };

        let mut likely = Vec::new();
        for ip in single_hosts.chain(lan).chain(first_of_subnet) {
            if Some(ip) != device && !ip.is_unspecified() && !likely.contains(&ip) {
                likely.push(ip);
            }
        }
        likely
    }

    /// This device's address inside the tunnel.
    fn device_address(&self) -> Option<IpAddr> {
        match &self.transport {
            Transport::WireGuard(wg) => wg.address.map(|address| address.ip),
        }
    }
}

fn max_prefix(ip: IpAddr) -> u8 {
    if ip.is_ipv4() { 32 } else { 128 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(address: &str, allowed_ips: &str) -> ClientConfig {
        read(&format!(
            "[Interface]\nAddress = {address}\n[Peer]\n\
             PublicKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n\
             Endpoint = vpn.example.net:51820\nAllowedIPs = {allowed_ips}\n"
        ))
        .unwrap()
    }

    fn likely(config: &ClientConfig, lan: &str) -> Vec<String> {
        config
            .likely_addresses(lan)
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn a_single_routed_address_comes_first_and_alone_when_nothing_else_is_routed() {
        let c = config("10.0.0.3/32", "10.0.0.1/32");
        assert_eq!(likely(&c, "192.0.2.10"), ["10.0.0.1"]);
    }

    #[test]
    fn a_routed_home_network_puts_the_lan_address_before_the_guess() {
        let c = config("10.0.0.3/32", "10.0.0.0/24, 192.0.2.0/24");
        assert_eq!(likely(&c, "192.0.2.10"), ["192.0.2.10", "10.0.0.1"]);
    }

    #[test]
    fn names_and_unrouted_addresses_are_not_tried() {
        let c = config("10.0.0.3/32", "10.0.0.0/24");
        assert_eq!(likely(&c, "git.example.com"), ["10.0.0.1"]);
        assert_eq!(likely(&c, "192.0.2.10"), ["10.0.0.1"]);
    }

    #[test]
    fn the_devices_own_address_is_never_a_guess() {
        let c = config("10.0.0.1/24", "10.0.0.0/24");
        assert!(likely(&c, "192.0.2.10").is_empty());
    }

    #[test]
    fn text_in_no_known_format_is_refused_naming_what_is_read() {
        let error = read("client\ndev tun\nremote vpn.example.net 1194\n").unwrap_err();
        assert!(matches!(error, ImportError::Unrecognised { .. }), "{error}");
        assert!(error.to_string().contains("WireGuard"), "{error}");
    }
}
