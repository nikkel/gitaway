//! `gitaway profile …`: add, list, show, remove, set the default, and change
//! single settings.
//!
//! `add` also creates the profile's device key, and `remove` deletes it (see
//! [`crate::keys`]).
//!
//! All file access goes through [`crate::config::store::Store`], so every
//! write is atomic and every change is validated before it's saved.
//!
//! # Extending
//!
//! A new profile field that `profile add` should ask for: add a flag to
//! [`super::args::AddArgs`], a prompt in the private `add` function, and set it in
//! `build_profile`.

use toml_edit::{DocumentMut, Item, Key, Table, Value, value};

use crate::address::HostPort;
use crate::config::name::ProfileName;
use crate::config::profile::Transport;
use crate::config::store::{Store, StoreError};
use crate::config::value::{HostKey, InterfaceAddress, WireGuardPublicKey};
use crate::error::Error;

use crate::keys::device_key::DeviceKey;
use crate::keys::store::KeyStore;

use super::args::{AddArgs, ProfileCommand};
use super::key_cmd::warn_if_fallback;
use super::prompt;

/// Runs a `gitaway profile` command.
pub fn run(command: ProfileCommand) -> Result<i32, Error> {
    let store = super::open_store()?;
    match command {
        ProfileCommand::Add(args) => add(&store, args),
        ProfileCommand::List => list(&store),
        ProfileCommand::Show { name } => show(&store, &parse_name(&name)?),
        ProfileCommand::Remove { name, yes } => remove(&store, &parse_name(&name)?, yes),
        ProfileCommand::Default { name } => set_default(&store, &parse_name(&name)?),
        ProfileCommand::Set { name, key, value } => set(&store, &parse_name(&name)?, &key, &value),
    }?;
    Ok(0)
}

/// Everything `profile add` collects, already validated.
struct NewProfile {
    endpoint: HostPort,
    server_public_key: WireGuardPublicKey,
    address: Option<InterfaceAddress>,
    home: Option<(HostPort, HostKey)>,
    hosts: Vec<(HostPort, HostPort)>,
}

fn add(store: &Store, args: AddArgs) -> Result<(), Error> {
    let name: ProfileName = prompt::required(args.name, "the profile name", "Profile name", |s| {
        s.parse().map_err(|e| format!("{e}"))
    })?;
    if store.profile_exists(&name) {
        return Err(StoreError::AlreadyExists(name).into());
    }

    let endpoint = prompt::required(
        args.endpoint,
        "--endpoint",
        "WireGuard server's public address (host:port)",
        parse_endpoint,
    )?;
    let server_public_key = prompt::required(
        args.server_public_key,
        "--server-public-key",
        "WireGuard server's public key",
        |s| s.parse::<WireGuardPublicKey>().map_err(|e| e.to_string()),
    )?;
    let address = prompt::optional(
        args.address,
        "--address",
        "This device's tunnel address, such as 10.0.0.2/32",
        |s| s.parse::<InterfaceAddress>().map_err(|e| e.to_string()),
    )?;

    let home_ssh = prompt::optional(
        args.home_ssh,
        "--home-ssh",
        "SSH server that proves you're home (host:port)",
        |s| s.parse::<HostPort>().map_err(|e| e.to_string()),
    )?;
    let home = match (home_ssh, args.host_key) {
        (Some(ssh), host_key) => {
            let question = format!(
                "Its host key (one line of `ssh-keyscan -p {} {}`, without the host name)",
                ssh.port.unwrap_or(22),
                ssh.host
            );
            let key = prompt::required(host_key, "--host-key", &question, |s| {
                s.parse::<HostKey>().map_err(|e| e.to_string())
            })?;
            Some((ssh, key))
        }
        (None, Some(_)) => return Err(Error::msg("--host-key needs --home-ssh")),
        (None, None) => None,
    };

    let mut hosts = args
        .hosts
        .iter()
        .map(|entry| {
            parse_host_entry(entry).map_err(|e| Error::msg(format!("--host {entry}: {e}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if hosts.is_empty()
        && let Some((ssh, _)) = &home
    {
        let lan = HostPort {
            host: ssh.host.clone(),
            port: None,
        };
        let question = format!("Address of {lan} through the tunnel, such as 10.0.0.1");
        if let Some(tunnel) = prompt::optional(None, "--host", &question, |s| {
            s.parse::<HostPort>()
                .map_err(|e| e.to_string())
                .and_then(|t| match (t.port, t.host.parse::<std::net::IpAddr>()) {
                    (None, Ok(_)) => Ok(t),
                    (Some(_), _) => Err("give an address without a port".to_owned()),
                    (None, Err(_)) => Err("give an IP address, such as 10.0.0.1".to_owned()),
                })
        })? {
            hosts.push((lan, tunnel));
        }
    }

    let new = NewProfile {
        endpoint,
        server_public_key,
        address,
        home,
        hosts,
    };
    let keys = KeyStore::from_env(store.dir().clone())?;
    store.create_profile(&name, &build_profile(&new).to_string())?;
    let key_message = match ensure_device_key(&keys, &name) {
        Ok(message) => message,
        Err(e) => {
            // Without a key the profile can't be used, so don't leave it half made.
            store.remove_profile(&name)?;
            return Err(e);
        }
    };
    if args.default {
        set_default(store, &name)?;
    }

    println!(
        "Created profile `{name}` ({}).",
        store.dir().profile_file(&name).display()
    );
    println!("{key_message}");
    warn_if_fallback(&keys, &name);
    println!("Run `gitaway peer {name}` to see what to add to your WireGuard server.");
    if let Some((_, key)) = &new.home {
        println!(
            "Home host key fingerprint: {}\n  Compare it on the server with `ssh-keygen -lf /etc/ssh/ssh_host_*_key.pub`.",
            key.fingerprint()
        );
    } else {
        println!(
            "No [home] section: gitaway can't tell when you're home, so it will refuse network commands for this profile."
        );
    }
    if new.hosts.is_empty() {
        println!(
            "No [hosts]: no remotes will use this profile. Add one with\n  gitaway profile set {name} 'hosts.\"192.0.2.10\"' 10.0.0.1"
        );
    }
    Ok(())
}

/// Creates the profile's device key, or keeps one left over from an earlier
/// profile with the same name (the server may already know it). Returns a
/// line saying which.
fn ensure_device_key(keys: &KeyStore, name: &ProfileName) -> Result<String, Error> {
    if let Some(existing) = keys.load(name)? {
        return Ok(format!(
            "Kept the existing device key for `{name}` (public key {}).",
            existing.public_key()
        ));
    }
    keys.save(name, &DeviceKey::generate())?;
    Ok(format!(
        "Created a device key, stored in {}.",
        keys.location(name)
    ))
}

/// Builds the profile file. Validation happens when the store saves it.
fn build_profile(new: &NewProfile) -> DocumentMut {
    let mut wireguard = Table::new();
    wireguard["endpoint"] = value(new.endpoint.to_string());
    wireguard["server_public_key"] = value(new.server_public_key.to_string());
    if let Some(address) = &new.address {
        wireguard["address"] = value(address.to_string());
    }
    let mut transport = Table::new();
    transport.set_implicit(true);
    transport["wireguard"] = Item::Table(wireguard);

    let mut doc = DocumentMut::new();
    doc["transport"] = Item::Table(transport);

    if let Some((ssh, host_key)) = &new.home {
        let mut home = Table::new();
        home["ssh"] = value(ssh.to_string());
        home["host_key"] = value(host_key.to_string());
        doc["home"] = Item::Table(home);
    }
    if !new.hosts.is_empty() {
        let mut hosts = Table::new();
        for (lan, tunnel) in &new.hosts {
            hosts[lan.to_string().as_str()] = value(tunnel.to_string());
        }
        doc["hosts"] = Item::Table(hosts);
    }
    doc
}

fn list(store: &Store) -> Result<(), Error> {
    let profiles = store.load_profiles()?;
    if profiles.is_empty() {
        println!("No profiles yet. Create one with `gitaway profile add`.");
        return Ok(());
    }
    let default = store.load_global()?.default_profile;
    for (name, profile) in &profiles {
        let marker = if default.as_ref() == Some(name) {
            "*"
        } else {
            " "
        };
        let Transport::WireGuard(wg) = &profile.transport;
        println!("{marker} {name}  (wireguard via {})", wg.endpoint);
    }
    Ok(())
}

fn show(store: &Store, name: &ProfileName) -> Result<(), Error> {
    // Loading first validates the file and gives a clear error if it's missing.
    let profile = store.load_profile(name)?;
    let path = store.dir().profile_file(name);
    let text = std::fs::read_to_string(&path).map_err(|source| StoreError::Io {
        path: path.clone(),
        source,
    })?;
    println!("# {}\n{}", path.display(), text.trim_end());
    if let Some(home) = &profile.home {
        println!(
            "\n# home host key fingerprint: {}",
            home.host_key.fingerprint()
        );
    }
    let keys = KeyStore::from_env(store.dir().clone())?;
    match keys.load(name)? {
        Some(key) => println!("# device public key: {}", key.public_key()),
        None => println!("# no device key: create one with `gitaway key rotate {name}`"),
    }
    Ok(())
}

fn remove(store: &Store, name: &ProfileName, yes: bool) -> Result<(), Error> {
    if !store.profile_exists(name) {
        return Err(StoreError::NotFound(name.clone()).into());
    }
    if !yes {
        if !prompt::interactive() {
            return Err(Error::msg(
                "pass --yes to remove a profile when not running in a terminal",
            ));
        }
        if !prompt::confirm(&format!("Remove profile `{name}` and its device key?"))? {
            println!("Nothing removed.");
            return Ok(());
        }
    }
    // The key goes first: if that fails, the profile is still there to retry with.
    let had_key = KeyStore::from_env(store.dir().clone())?.delete(name)?;
    store.remove_profile(name)?;
    let what = if had_key {
        format!("profile `{name}` and its device key")
    } else {
        format!("profile `{name}`")
    };
    if store.load_global()?.default_profile.as_ref() == Some(name) {
        store.update_global(|doc| {
            doc.remove("default_profile");
            Ok(())
        })?;
        println!("Removed {what}. It was the default, so there's no default now.");
    } else {
        println!("Removed {what}.");
    }
    Ok(())
}

fn set_default(store: &Store, name: &ProfileName) -> Result<(), Error> {
    if !store.profile_exists(name) {
        return Err(StoreError::NotFound(name.clone()).into());
    }
    store.update_global(|doc| {
        doc["default_profile"] = value(name.as_str());
        Ok(())
    })?;
    println!("`{name}` is now the default profile.");
    Ok(())
}

fn set(store: &Store, name: &ProfileName, key: &str, raw_value: &str) -> Result<(), Error> {
    let path = Key::parse(key).map_err(|e| Error::msg(format!("invalid key `{key}`: {e}")))?;
    let new_value = parse_value(raw_value);
    store.update_profile(name, |doc| set_path(doc, &path, new_value))?;
    println!("Updated `{key}` in profile `{name}`.");
    Ok(())
}

/// Sets a dotted key path in the document, creating tables as needed.
fn set_path(doc: &mut DocumentMut, path: &[Key], new_value: Value) -> Result<(), String> {
    let (last, parents) = path.split_last().ok_or("the key is empty")?;
    let mut table = doc.as_table_mut();
    for part in parents {
        let entry = table.entry(part.get()).or_insert_with(|| {
            let mut child = Table::new();
            child.set_implicit(true);
            Item::Table(child)
        });
        table = entry.as_table_mut().ok_or_else(|| {
            format!(
                "`{}` isn't a table, so it can't contain other settings",
                part.get()
            )
        })?;
    }
    table[last.get()] = Item::Value(new_value);
    Ok(())
}

/// Reads a value as TOML if it is one (`1420`, `true`, `"text"`), otherwise
/// as a plain string, so `192.0.2.10:22` needs no quotes.
fn parse_value(raw: &str) -> Value {
    match raw.parse::<Value>() {
        Ok(mut parsed) => {
            parsed.decor_mut().clear();
            parsed
        }
        Err(_) => Value::from(raw),
    }
}

fn parse_name(name: &str) -> Result<ProfileName, Error> {
    name.parse().map_err(|e| Error::msg(format!("{e}")))
}

fn parse_endpoint(s: &str) -> Result<HostPort, String> {
    let endpoint: HostPort = s.parse().map_err(|e| format!("{e}"))?;
    if endpoint.port.is_none() {
        return Err(format!("`{s}` needs a port, such as {s}:51820"));
    }
    Ok(endpoint)
}

/// `LAN=TUNNEL`, both host or both host:port.
fn parse_host_entry(entry: &str) -> Result<(HostPort, HostPort), String> {
    let (lan, tunnel) = entry.split_once('=').ok_or("expected LAN=TUNNEL")?;
    let lan: HostPort = lan.trim().parse().map_err(|e| format!("{e}"))?;
    let tunnel: HostPort = tunnel.trim().parse().map_err(|e| format!("{e}"))?;
    if lan.port.is_some() != tunnel.port.is_some() {
        return Err("give both sides a port, or neither".to_owned());
    }
    Ok((lan, tunnel))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_typed_when_they_are_toml() {
        assert_eq!(parse_value("1420").as_integer(), Some(1420));
        assert_eq!(parse_value("true").as_bool(), Some(true));
        assert_eq!(parse_value("\"quoted\"").as_str(), Some("quoted"));
        assert_eq!(parse_value("192.0.2.10:22").as_str(), Some("192.0.2.10:22"));
        assert_eq!(
            parse_value("ssh-ed25519 AAAA x").as_str(),
            Some("ssh-ed25519 AAAA x")
        );
    }

    #[test]
    fn set_path_creates_tables_and_refuses_non_tables() {
        let mut doc: DocumentMut = "[home]\nssh = \"192.0.2.10\"\n".parse().unwrap();
        let path = Key::parse("hosts.\"192.0.2.10\"").unwrap();
        set_path(&mut doc, &path, Value::from("10.0.0.1")).unwrap();
        assert_eq!(doc["hosts"]["192.0.2.10"].as_str(), Some("10.0.0.1"));

        let through_string = Key::parse("home.ssh.port").unwrap();
        assert!(set_path(&mut doc, &through_string, Value::from(22)).is_err());
    }

    #[test]
    fn host_entries() {
        let (lan, tunnel) = parse_host_entry("192.0.2.10=10.0.0.1").unwrap();
        assert_eq!(
            (lan.to_string(), tunnel.to_string()),
            ("192.0.2.10".into(), "10.0.0.1".into())
        );
        assert!(parse_host_entry("192.0.2.10:22=10.0.0.1:22").is_ok());
        for bad in [
            "192.0.2.10",
            "192.0.2.10:22=10.0.0.1",
            "=10.0.0.1",
            "bad host=10.0.0.1",
        ] {
            assert!(parse_host_entry(bad).is_err(), "{bad}");
        }
    }
}
