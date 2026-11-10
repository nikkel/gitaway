//! `gitaway check`: test a profile's tunnel end to end, and pin its home
//! server's SSH host key through it.
//!
//! ```text
//!  [home].ssh ──[hosts]──▶ address through the tunnel
//!  take the lock, load the device key, open the tunnel
//!  connect to that address ──── fails? ──▶ say which part (no handshake, …)
//!  SSH key exchange: the server proves its host key
//!    nothing pinned ──▶ show it, pin it (asks, or --yes)
//!    same as pinned ──▶ fine
//!    different ───────▶ warn, replace only if confirmed (or --yes)
//!  run the ordinary home check from here and report it
//! ```
//!
//! The home check runs first. If it passes, the pinned key has just been
//! verified directly, and the tunnel isn't tried: from inside the home
//! network it would have to go out to the router's own public address and
//! back in, which many routers don't do ("hairpin NAT"), and it wouldn't
//! show how the tunnel behaves away anyway.
//!
//! Capturing through the tunnel needs no trust on first use: WireGuard has
//! already proved the far end is the profile's server, whatever network
//! this device is on. `ssh-keyscan` on a LAN has no such guarantee.

use std::net::SocketAddr;
use std::time::Duration;

use toml_edit::value;

use crate::config::name::ProfileName;
use crate::config::profile::{Home, Profile};
use crate::config::store::Store;
use crate::config::value::HostKey;
use crate::error::Error;
use crate::home_check::{self, AwayReason, Timeouts, Verdict};
use crate::keys::store::DeviceSecrets;
use crate::transport;

use super::{away, prompt};

/// How long the server may take to prove its key, through the tunnel.
pub(crate) const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs `gitaway check`. Exits 0 when a key is pinned that matches the
/// server, proved directly at home or through a working tunnel; 1 otherwise.
pub fn run(store: &Store, name: &ProfileName, yes: bool) -> Result<i32, Error> {
    let profile = store.load_profile(name)?;
    let Some(home) = profile.home.clone() else {
        return Err(Error::msg(format!(
            "profile `{name}` has no [home] SSH server to check. Add one with \
             `gitaway profile set {name} home.ssh <host:port>`"
        )));
    };
    let target = profile
        .hosts
        .route(&home.ssh.host, home.ssh_port())
        .ok_or_else(|| {
            Error::msg(format!(
                "profile `{name}`'s [hosts] doesn't say where {} is through the tunnel. Add it \
                 with `gitaway profile set {name} 'hosts.\"{}\"' <address through the tunnel>`",
                home.ssh, home.ssh.host
            ))
        })?;

    // At home the device key isn't needed, so the keychain isn't touched.
    if home_check::check(&home)? == Verdict::Home {
        println!(
            "Home check: you're home, and {} proved the pinned host key directly, so \
             commands connect to it directly.\n\
             Tunnel: not tested at home. From inside the home network it would need your \
             router to reach its own public address, which many can't (no \"hairpin NAT\"). \
             Run `gitaway check {name}` on another network to test it.",
            home.ssh
        );
        return Ok(0);
    }

    let (_lock, secrets) = away::take_tunnel(store, name)?;
    let runtime = away::runtime()?;
    let result = runtime.block_on(async {
        let seen = capture_through_tunnel(name, &profile, &home, target, secrets)
            .await
            .map_err(|error| {
                Error::msg(format!(
                    "{error}\nIf you're on the home network, your router may not be able to reach \
                     its own public address, so the tunnel can only be tested from another network."
                ))
            })?;
        let (home, ok) = settle_key(store, name, home, seen, yes)?;
        report_home_check(&home).await;
        Ok(if ok { 0 } else { 1 })
    });
    runtime.shutdown_timeout(away::SHUTDOWN_GRACE);
    result
}

/// Opens the tunnel, reaches the home server at `target` through it, and
/// returns the host key it proves.
async fn capture_through_tunnel(
    name: &ProfileName,
    profile: &Profile,
    home: &Home,
    target: SocketAddr,
    secrets: DeviceSecrets,
) -> Result<HostKey, Error> {
    let transport = transport::open(name.as_str(), profile, secrets).await?;
    let stream = transport.connect(target).await?;
    println!(
        "Tunnel: works. Reached {} through it, at {target}.",
        home.ssh
    );
    let pinned = home.host_key.as_ref();
    home_check::capture(stream, CAPTURE_TIMEOUT, pinned.map(HostKey::algorithm))
        .await
        .map_err(|e| Error::msg(format!("{} answered through the tunnel, but {e}", home.ssh)))
}

/// Compares the key the server proved with the pinned one, and pins it if
/// that's wanted. Returns the home settings now in force and whether a
/// matching key is pinned.
fn settle_key(
    store: &Store,
    name: &ProfileName,
    mut home: Home,
    seen: HostKey,
    yes: bool,
) -> Result<(Home, bool), Error> {
    println!(
        "Home server's host key: {} {}",
        algorithm_name(&seen),
        seen.fingerprint()
    );
    let question = match &home.host_key {
        Some(pinned) if pinned.same_key(&seen) => {
            println!("It matches the key pinned in profile `{name}`.");
            return Ok((home, true));
        }
        Some(pinned) => {
            println!(
                "WARNING: it differs from the key pinned in profile `{name}` ({}).\n\
                 The tunnel proves this is your own network, so either the server's key changed \
                 (a reinstall, say), or [hosts] sends {} to a different machine than the one at \
                 home. Check on the server with `ssh-keygen -lf /etc/ssh/ssh_host_*_key.pub`.",
                pinned.fingerprint(),
                home.ssh
            );
            "Replace the pinned key with this one?"
        }
        None => "Pin this key, so gitaway can recognise home?",
    };
    if !yes && !(prompt::interactive() && prompt::confirm(question)?) {
        println!("Left profile `{name}` unchanged. Pass --yes to pin the key without asking.");
        return Ok((home, false));
    }
    store.update_profile(name, |doc| {
        doc["home"]["host_key"] = value(seen.to_string());
        Ok(())
    })?;
    println!("Pinned it in profile `{name}`.");
    home.host_key = Some(seen);
    Ok((home, true))
}

/// Runs the ordinary home check, as a network command would, and says what
/// it means.
async fn report_home_check(home: &Home) {
    match home_check::verify(home, Timeouts::default()).await {
        Verdict::Home => println!(
            "Home check: you're home, so commands connect to {} directly.",
            home.ssh
        ),
        Verdict::Away(AwayReason::NotPinned) => {
            println!("Home check: no key pinned, so commands always use the tunnel.");
        }
        Verdict::Away(reason) => {
            println!("Home check: away from home ({reason}), so commands use the tunnel.");
            if matches!(
                reason,
                AwayReason::ConnectTimeout(_) | AwayReason::Unreachable(_)
            ) {
                println!(
                    "  If you are home right now, {} isn't answering this device directly, though it \
                     answered through the tunnel. A firewall or an auto-block tool (fail2ban, a \
                     NAS's login protection) may have banned this device's address: the home check's \
                     connections never log in, and some tools count them as failed logins. Allow \
                     your home network in them.",
                    home.ssh
                );
            }
        }
    }
}

/// The key's type as it's written in the key, such as `ssh-ed25519`.
pub(crate) fn algorithm_name(key: &HostKey) -> String {
    key.to_string()
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned()
}
