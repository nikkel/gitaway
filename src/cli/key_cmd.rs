//! `gitaway peer`, `gitaway key rotate` and `gitaway key import`: what the
//! server needs to know about this device, and replacing the device key.
//!
//! The `[Peer]` block goes to stdout and everything else to stderr, so
//! `gitaway peer home > peer.conf` saves just the block.
//!
//! `peer` never creates a key. Only `key rotate` and `key import` replace
//! one, and both ask first, so a key can't change by accident. An imported
//! key is read from stdin only: a command-line argument would show up in
//! `ps` and the shell's history.

use std::io::Read;
use std::net::IpAddr;

use zeroize::Zeroizing;

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

/// The most `key import` reads from stdin: far more than any WireGuard
/// config, so a wrong pipe fails quickly instead of filling memory.
const IMPORT_LIMIT: u64 = 64 * 1024;

/// Runs `gitaway key import`.
pub fn import(
    store: &crate::config::store::Store,
    name: &ProfileName,
    yes: bool,
) -> Result<i32, Error> {
    let profile = store.load_profile(name)?;
    let keys = KeyStore::from_env(store.dir().clone())?;
    let replacing = keys.load(name)?.is_some();
    if replacing && !yes && !prompt::interactive() {
        return Err(Error::msg(format!(
            "profile `{name}` already has a device key. Pass --yes to replace it \
             when the key comes from a pipe"
        )));
    }

    let text = read_private_key_text()?;
    let key = parse_private_key(&text).map_err(Error::msg)?;
    let Transport::WireGuard(wg) = &profile.transport;
    if key.public_key() == wg.server_public_key {
        return Err(Error::msg(
            "that's the VPN server's own private key, which must never leave the server. \
             Use this device's key: the PrivateKey under [Interface] in its client config",
        ));
    }

    if replacing && !yes {
        let question = format!(
            "Replace the device key for `{name}`? The tunnel will only connect if the server has the new public key"
        );
        if !prompt::confirm(&question)? {
            eprintln!("Nothing changed.");
            return Ok(0);
        }
    }
    keys.save(name, &key)?;
    warn_if_fallback(&keys, name);
    eprintln!(
        "Imported the device key for `{name}`, stored in {}.\n\
         The server needs this peer. If another device or app uses the same key, don't run\n\
         them at the same time: the server keeps one session per key.\n",
        keys.location(name)
    );
    print_peer_block(name, &profile, &key.public_key());
    Ok(0)
}

/// Reads the key from stdin: pasted at a hidden prompt in a terminal, or
/// everything piped in.
fn read_private_key_text() -> Result<Zeroizing<String>, Error> {
    if prompt::interactive() {
        return prompt::secret("Private key (input hidden)");
    }
    let mut text = Zeroizing::new(String::new());
    std::io::stdin()
        .take(IMPORT_LIMIT + 1)
        .read_to_string(&mut text)
        .map_err(|e| Error::msg(format!("couldn't read the key from stdin: {e}")))?;
    if text.len() as u64 > IMPORT_LIMIT {
        return Err(Error::msg(
            "stdin is too long to be a key or a WireGuard config",
        ));
    }
    Ok(text)
}

/// Finds the private key in `text`: a bare base64 key (as `wg genkey`
/// prints), or a WireGuard config with exactly one `PrivateKey` line.
/// Errors never include the text, which may hold a secret.
fn parse_private_key(text: &str) -> Result<DeviceKey, String> {
    let mut found = text.lines().filter_map(private_key_line);
    let candidate = match (found.next(), found.next()) {
        (Some(value), None) => value,
        (Some(_), Some(_)) => {
            return Err(
                "found more than one PrivateKey line; give the config of just this device"
                    .to_owned(),
            );
        }
        (None, _) => text.trim(),
    };
    if candidate.is_empty() {
        return Err("no key given".to_owned());
    }
    DeviceKey::from_base64(candidate).map_err(|_| {
        "that isn't a WireGuard private key (44 characters of base64, as `wg genkey` prints)"
            .to_owned()
    })
}

/// The value of a `PrivateKey = …` line. WireGuard config keys are
/// case-insensitive.
fn private_key_line(line: &str) -> Option<&str> {
    let (key, value) = line.split_once('=')?;
    key.trim()
        .eq_ignore_ascii_case("PrivateKey")
        .then(|| value.trim())
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
pub(crate) fn print_peer_block(
    name: &ProfileName,
    profile: &Profile,
    public_key: &WireGuardPublicKey,
) {
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

    /// RFC 7748 section 6.1: Alice's private and public keys.
    const PRIVATE: &str = "dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo=";
    const PUBLIC: &str = "hSDwCYkwp1R0i33ctD73Wg2/Og0mOBr066SpjqqbTmo=";

    fn public_of(text: &str) -> Result<String, String> {
        parse_private_key(text).map(|key| key.public_key().to_string())
    }

    #[test]
    fn a_bare_key_is_read_with_or_without_whitespace() {
        assert_eq!(public_of(PRIVATE).unwrap(), PUBLIC);
        assert_eq!(public_of(&format!("  {PRIVATE}\n")).unwrap(), PUBLIC);
    }

    #[test]
    fn the_private_key_is_found_in_a_wireguard_config() {
        let config = format!(
            "[Interface]\nprivatekey={PRIVATE}\nAddress = 10.0.0.2/32\n\n\
             [Peer]\nPublicKey = {PUBLIC}\nEndpoint = vpn.example.net:51820\n"
        );
        assert_eq!(public_of(&config).unwrap(), PUBLIC);
    }

    #[test]
    fn ambiguous_empty_or_damaged_input_is_refused_without_echoing_it() {
        let two = format!("PrivateKey = {PRIVATE}\nPrivateKey = {PRIVATE}\n");
        let damaged = format!("[Interface]\nPrivateKey = {}\n", &PRIVATE[..40]);
        for bad in [two.as_str(), "", "  \n", "not a key", damaged.as_str()] {
            let error = public_of(bad).unwrap_err();
            assert!(!error.contains(&PRIVATE[..20]), "secret echoed: {error}");
        }
    }

    #[test]
    fn allowed_ips_is_a_single_host() {
        assert_eq!(single_host("10.0.0.2".parse().unwrap()), "10.0.0.2/32");
        assert_eq!(single_host("fd00::2".parse().unwrap()), "fd00::2/128");
    }
}
