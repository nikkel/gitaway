//! `gitaway peer` and `gitaway key rotate`: what the server needs to know
//! about this device, and replacing the device key.
//!
//! The `[Peer]` block goes to stdout and everything else to stderr, so
//! `gitaway peer home > peer.conf` saves just the block.
//!
//! `peer` never creates a key, and `key rotate` is the only command that
//! replaces one, so a key can't change by accident.

use std::net::IpAddr;

use crate::config::name::ProfileName;
use crate::config::profile::{Profile, Transport};
use crate::config::value::WireGuardPublicKey;
use crate::error::Error;
use crate::keys::device_key::DeviceKey;
use crate::keys::store::KeyStore;

use super::prompt;

/// Runs `gitaway peer`.
pub fn peer(store: &crate::config::store::Store, name: &ProfileName) -> Result<i32, Error> {
    let profile = store.load_profile(name)?;
    let keys = KeyStore::from_env(store.dir().clone())?;
    let Some(key) = keys.load(name)? else {
        return Err(Error::msg(format!(
            "profile `{name}` has no device key. Create one with `gitaway key rotate {name}`"
        )));
    };
    eprintln!("Add this peer to the WireGuard server for profile `{name}`:\n");
    print_peer_block(name, &profile, &key.public_key());
    Ok(0)
}

/// Runs `gitaway key rotate`.
pub fn rotate(
    store: &crate::config::store::Store,
    name: &ProfileName,
    yes: bool,
) -> Result<i32, Error> {
    let profile = store.load_profile(name)?;
    let keys = KeyStore::from_env(store.dir().clone())?;
    let replacing = keys.load(name)?.is_some();
    if replacing && !yes {
        if !prompt::interactive() {
            return Err(Error::msg(
                "pass --yes to replace a device key when not running in a terminal",
            ));
        }
        let question = format!(
            "Replace the device key for `{name}`? The tunnel won't connect until the server has the new public key"
        );
        if !prompt::confirm(&question)? {
            eprintln!("Nothing changed.");
            return Ok(0);
        }
    }

    let key = DeviceKey::generate();
    keys.save(name, &key)?;
    warn_if_fallback(&keys, name);
    let action = if replacing { "Replaced" } else { "Created" };
    eprintln!(
        "{action} the device key for `{name}`, stored in {}.\n\
         Give the server this peer, replacing any old one for this device:\n",
        keys.location(name)
    );
    print_peer_block(name, &profile, &key.public_key());
    Ok(0)
}

/// Warns that a key went to a file because no keychain was found.
pub(crate) fn warn_if_fallback(keys: &KeyStore, name: &ProfileName) {
    if keys.fell_back_to_file() {
        eprintln!(
            "gitaway: warning: no OS keychain found, so the device key is in {} (readable only by you).",
            keys.location(name)
        );
    }
}

/// Prints the `[Peer]` block for the server, with guidance on stderr when
/// the tunnel address isn't known yet.
fn print_peer_block(name: &ProfileName, profile: &Profile, public_key: &WireGuardPublicKey) {
    let Transport::WireGuard(wg) = &profile.transport;
    println!("[Peer]");
    println!("# gitaway profile `{name}`");
    println!("PublicKey = {public_key}");
    match &wg.address {
        Some(address) => println!("AllowedIPs = {}", single_host(address.ip)),
        None => {
            println!("AllowedIPs = <a free tunnel address for this device>/32");
            eprintln!(
                "\nThis profile has no tunnel address yet. Pick a free one on the server, put it in\n\
                 AllowedIPs, then save it here with\n  \
                 gitaway profile set {name} transport.wireguard.address <address>/32"
            );
        }
    }
}

/// The server routes exactly this device's address to it, whatever prefix
/// the device's own interface uses.
fn single_host(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => format!("{ip}/32"),
        IpAddr::V6(ip) => format!("{ip}/128"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_ips_is_a_single_host() {
        assert_eq!(single_host("10.0.0.2".parse().unwrap()), "10.0.0.2/32");
        assert_eq!(single_host("fd00::2".parse().unwrap()), "fd00::2/128");
    }
}
