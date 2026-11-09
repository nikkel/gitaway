//! Reading a WireGuard client config: the `.conf` file a WireGuard server
//! or app gives a device, as the WireGuard apps import it.
//!
//! ```ini
//! [Interface]                  # this device
//! PrivateKey = <base64>        # → the keychain
//! Address = 10.0.0.2/32        # → transport.wireguard.address
//! MTU = 1420                   # → transport.wireguard.mtu (optional)
//!
//! [Peer]                       # the server
//! PublicKey = <base64>         # → transport.wireguard.server_public_key
//! Endpoint = vpn.example.net:51820   # → transport.wireguard.endpoint
//! AllowedIPs = 10.0.0.1/32     # → where the git server may be in the tunnel
//! ```
//!
//! Keys are case-insensitive and `#` starts a comment, as in `wg-quick`.
//! Settings gitaway has no use for (`DNS`, `PersistentKeepalive`, `PostUp`,
//! …) are ignored. A config with several `[Peer]` sections, or a
//! `PresharedKey`, is refused: gitaway connects to one server, and doesn't
//! keep preshared keys yet.

use std::net::IpAddr;

use ipnetwork::IpNetwork;

use crate::address::HostPort;
use crate::config::profile::{Transport, WireGuard};
use crate::config::value::{InterfaceAddress, WireGuardPublicKey};
use crate::keys::device_key::DeviceKey;
use crate::transport::client_config::{ClientConfig, ConfigFormat};

/// The WireGuard client config format.
pub struct WireGuardConfig;

impl ConfigFormat for WireGuardConfig {
    fn name(&self) -> &'static str {
        "WireGuard"
    }

    fn recognises(&self, text: &str) -> bool {
        text.lines()
            .filter_map(|line| section_name(content(line)))
            .any(|name| name == "interface" || name == "peer")
    }

    fn read(&self, text: &str) -> Result<ClientConfig, String> {
        Fields::collect(text)?.into_config()
    }
}

/// Which section a line is in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Interface,
    Peer,
}

/// The settings gitaway uses, as found, before they're checked.
#[derive(Default)]
struct Fields {
    interfaces: usize,
    peers: usize,
    private_key: Option<DeviceKey>,
    addresses: Vec<String>,
    mtu: Option<String>,
    public_key: Option<String>,
    endpoint: Option<String>,
    allowed_ips: Vec<String>,
    preshared_key: bool,
}

impl Fields {
    /// Collects the settings, line by line. Error messages name lines by
    /// number and never quote them: a line may hold the private key.
    fn collect(text: &str) -> Result<Self, String> {
        let mut fields = Fields::default();
        let mut section = None;
        for (index, line) in text.lines().enumerate() {
            let number = index + 1;
            let line = content(line);
            if line.is_empty() {
                continue;
            }
            if let Some(name) = section_name(line) {
                section = Some(match name.as_str() {
                    "interface" => {
                        fields.interfaces += 1;
                        Section::Interface
                    }
                    "peer" => {
                        fields.peers += 1;
                        Section::Peer
                    }
                    _ => return Err(format!("line {number} starts an unknown section")),
                });
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!("line {number} isn't a `Key = value` setting"));
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            match (section, key.as_str()) {
                (None, _) => {
                    return Err(format!(
                        "line {number} comes before the [Interface] and [Peer] sections"
                    ));
                }
                (Some(Section::Interface), "privatekey") => {
                    if fields.private_key.is_some() {
                        return Err("it has more than one PrivateKey".to_owned());
                    }
                    let key = DeviceKey::from_base64(value).map_err(|_| {
                        format!("the PrivateKey on line {number} isn't a WireGuard private key")
                    })?;
                    fields.private_key = Some(key);
                }
                (Some(Section::Interface), "address") => fields.addresses.extend(list(value)),
                (Some(Section::Interface), "mtu") => fields.mtu = Some(value.to_owned()),
                (Some(Section::Peer), "publickey") => fields.public_key = Some(value.to_owned()),
                (Some(Section::Peer), "endpoint") => fields.endpoint = Some(value.to_owned()),
                (Some(Section::Peer), "allowedips") => fields.allowed_ips.extend(list(value)),
                (Some(Section::Peer), "presharedkey") => fields.preshared_key = true,
                _ => {}
            }
        }
        Ok(fields)
    }

    /// Checks the settings and turns them into a profile's.
    fn into_config(self) -> Result<ClientConfig, String> {
        match (self.interfaces, self.peers) {
            (0, _) => return Err("it has no [Interface] section (this device)".to_owned()),
            (1, 0) => return Err("it has no [Peer] section (the server)".to_owned()),
            (1, 1) => {}
            (1, peers) => {
                return Err(format!(
                    "it has {peers} [Peer] sections, but gitaway connects to one server. \
                     Use a config with just the server that has your git server"
                ));
            }
            _ => return Err("it has more than one [Interface] section".to_owned()),
        }
        if self.preshared_key {
            return Err(
                "it uses a PresharedKey, which gitaway doesn't support yet. \
                 Ask the server for a config without one"
                    .to_owned(),
            );
        }
        let server_public_key: WireGuardPublicKey = self
            .public_key
            .ok_or("the [Peer] section has no PublicKey (the server's key)")?
            .parse()
            .map_err(|_| "the [Peer] PublicKey isn't a WireGuard public key".to_owned())?;
        if let Some(key) = &self.private_key
            && key.public_key() == server_public_key
        {
            return Err(
                "the [Peer] PublicKey belongs to this device's own PrivateKey; \
                 it should be the server's key"
                    .to_owned(),
            );
        }
        let endpoint = parse_endpoint(
            self.endpoint
                .as_deref()
                .ok_or("the [Peer] section has no Endpoint (the server's public address)")?,
        )?;
        let address = pick_address(&self.addresses)?;
        let mtu = self
            .mtu
            .map(|mtu| {
                mtu.parse::<u16>()
                    .map_err(|_| format!("MTU `{mtu}` isn't a number"))
            })
            .transpose()?;
        let routes = self
            .allowed_ips
            .iter()
            .map(|route| {
                route
                    .parse::<IpNetwork>()
                    .map_err(|_| format!("AllowedIPs `{route}` isn't an address or network"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if routes.is_empty() {
            return Err("the [Peer] section has no AllowedIPs".to_owned());
        }

        Ok(ClientConfig {
            transport: Transport::WireGuard(WireGuard {
                endpoint,
                server_public_key,
                address: Some(address),
                mtu,
            }),
            key: self.private_key,
            routes,
        })
    }
}

/// A line without its comment and surrounding whitespace.
fn content(line: &str) -> &str {
    line.split('#').next().unwrap_or_default().trim()
}

/// The lower-cased name of a `[Section]` line, or `None` for other lines.
fn section_name(line: &str) -> Option<String> {
    let name = line.strip_prefix('[')?.strip_suffix(']')?;
    Some(name.trim().to_ascii_lowercase())
}

/// The items of a comma-separated value.
fn list(value: &str) -> impl Iterator<Item = String> + '_ {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
}

fn parse_endpoint(text: &str) -> Result<HostPort, String> {
    let endpoint: HostPort = text
        .parse()
        .map_err(|_| format!("Endpoint `{text}` isn't a host:port"))?;
    if endpoint.port.is_none() {
        return Err(format!("Endpoint `{text}` has no port"));
    }
    Ok(endpoint)
}

/// This device's tunnel address. gitaway's tunnel has one address, so with
/// several (IPv4 and IPv6, say) the first IPv4 one is used: home servers
/// are nearly always reachable over IPv4. A bare address means a single
/// host, as with `wg-quick`.
fn pick_address(addresses: &[String]) -> Result<InterfaceAddress, String> {
    let parsed = addresses
        .iter()
        .map(|text| {
            text.parse::<InterfaceAddress>()
                .or_else(|_| {
                    text.parse::<IpAddr>().map(|ip| InterfaceAddress {
                        ip,
                        prefix: if ip.is_ipv4() { 32 } else { 128 },
                    })
                })
                .map_err(|_| format!("Address `{text}` isn't an address such as 10.0.0.2/32"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    parsed
        .iter()
        .find(|address| address.ip.is_ipv4())
        .or(parsed.first())
        .copied()
        .ok_or_else(|| {
            "the [Interface] section has no Address (this device's tunnel address)".to_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7748 section 6.1: Alice's private and public keys, and Bob's
    /// public key, standing in for the device and the server.
    const DEVICE_PRIVATE: &str = "dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo=";
    const DEVICE_PUBLIC: &str = "hSDwCYkwp1R0i33ctD73Wg2/Og0mOBr066SpjqqbTmo=";
    const SERVER_PUBLIC: &str = "3p7bfXt9wbTTW2HC7OQ1Nz+DQ8hbeGdNrfx+FG+IK08=";

    fn read(text: &str) -> Result<ClientConfig, String> {
        assert!(WireGuardConfig.recognises(text), "not recognised");
        WireGuardConfig.read(text)
    }

    fn wireguard(config: &ClientConfig) -> &WireGuard {
        let Transport::WireGuard(wg) = &config.transport;
        wg
    }

    /// A config as a WireGuard server hands it out.
    fn server_made() -> String {
        format!(
            "[Interface]\nPrivateKey = {DEVICE_PRIVATE}\nAddress = 10.0.0.3/32\n\n\
             [Peer]\nPublicKey = {SERVER_PUBLIC}\nAllowedIPs = 10.0.0.1/32\n\
             Endpoint = vpn.example.net:51820\nPersistentKeepalive = 25\n"
        )
    }

    #[test]
    fn a_server_made_config_fills_the_whole_transport_and_the_key() {
        let config = read(&server_made()).unwrap();
        let wg = wireguard(&config);
        assert_eq!(wg.endpoint.to_string(), "vpn.example.net:51820");
        assert_eq!(wg.server_public_key.to_string(), SERVER_PUBLIC);
        assert_eq!(wg.address.unwrap().to_string(), "10.0.0.3/32");
        assert_eq!(wg.mtu, None);
        let key = config.key.as_ref().expect("the private key is read");
        assert_eq!(key.public_key().to_string(), DEVICE_PUBLIC);
        assert_eq!(config.routes, ["10.0.0.1/32".parse().unwrap()]);
    }

    #[test]
    fn case_comments_lists_and_extra_settings_are_handled_like_wg_quick() {
        let text = format!(
            "# made by the server\n[interface]\nprivatekey={DEVICE_PRIVATE}\n\
             ADDRESS = fd00::3/128, 10.0.0.3/24 # both families\nDNS = 10.0.0.1\nMTU = 1380\n\
             [PEER]\npublickey = {SERVER_PUBLIC}\nendpoint = [2001:db8::1]:51820\n\
             allowedips = 10.0.0.0/24\nAllowedIPs = 192.0.2.0/24\n"
        );
        let config = read(&text).unwrap();
        let wg = wireguard(&config);
        assert_eq!(wg.address.unwrap().to_string(), "10.0.0.3/24");
        assert_eq!(wg.mtu, Some(1380));
        assert_eq!(wg.endpoint.port, Some(51820));
        assert_eq!(wg.endpoint.host, "2001:db8::1");
        assert_eq!(config.routes.len(), 2);
    }

    #[test]
    fn a_config_without_a_private_key_still_reads() {
        let text = server_made().replace(&format!("PrivateKey = {DEVICE_PRIVATE}\n"), "");
        assert!(read(&text).unwrap().key.is_none());
    }

    #[test]
    fn a_bare_address_is_a_single_host() {
        let text = server_made().replace("10.0.0.3/32", "10.0.0.3");
        let config = read(&text).unwrap();
        assert_eq!(
            wireguard(&config).address.unwrap().to_string(),
            "10.0.0.3/32"
        );
    }

    #[test]
    fn missing_or_wrong_settings_are_refused_with_the_reason() {
        let base = server_made();
        let cases = [
            (
                base.replace("[Peer]", "[Peer]\n[Peer]"),
                "2 [Peer] sections",
            ),
            (base.replace("[Interface]", ""), "before the [Interface]"),
            (
                base.replace("Endpoint = vpn.example.net:51820", ""),
                "no Endpoint",
            ),
            (
                base.replace("vpn.example.net:51820", "vpn.example.net"),
                "has no port",
            ),
            (base.replace("Address = 10.0.0.3/32", ""), "no Address"),
            (
                base.replace("AllowedIPs = 10.0.0.1/32", ""),
                "no AllowedIPs",
            ),
            (
                base.replace(SERVER_PUBLIC, "not-a-key"),
                "isn't a WireGuard public key",
            ),
            (
                base.replace(SERVER_PUBLIC, DEVICE_PUBLIC),
                "should be the server's key",
            ),
            (
                base.replace(
                    "[Peer]",
                    "[Peer]\nPresharedKey = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                ),
                "PresharedKey",
            ),
            (base.replace("[Peer]", "[Peer]\nnonsense"), "line 6 isn't"),
        ];
        for (text, expected) in cases {
            let error = WireGuardConfig.read(&text).err().unwrap_or_default();
            assert!(
                error.contains(expected),
                "expected {expected:?}, got {error:?}"
            );
        }
    }

    #[test]
    fn errors_never_quote_the_private_key() {
        let damaged = server_made().replace(DEVICE_PRIVATE, &DEVICE_PRIVATE[..40]);
        let twice = server_made().replace(
            "Address",
            &format!("PrivateKey = {DEVICE_PRIVATE}\nAddress"),
        );
        let stray = format!("{DEVICE_PRIVATE}\n{}", server_made());
        for text in [damaged, twice, stray] {
            let error = WireGuardConfig.read(&text).expect_err("refused");
            assert!(
                !error.contains(&DEVICE_PRIVATE[..20]),
                "secret echoed: {error}"
            );
        }
    }

    #[test]
    fn only_text_with_wireguard_sections_is_recognised() {
        assert!(WireGuardConfig.recognises(&server_made()));
        assert!(!WireGuardConfig.recognises("client\nremote vpn.example.net 1194\n"));
        assert!(!WireGuardConfig.recognises("# [Interface] in a comment only\n"));
    }
}
