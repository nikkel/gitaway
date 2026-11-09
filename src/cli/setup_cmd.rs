//! `gitaway setup`: make a profile from the VPN config a server gave this
//! device and a clone URL, and work out the rest by testing.
//!
//! ```text
//!  VPN config ──▶ transport settings, device key, routes   (transport::client_config)
//!  clone URL ───▶ the git server's address at home, and its SSH port
//!  that SSH server answers on this network, and you say you're home?
//!     yes ──▶ pin the key it proves; the tunnel isn't tested
//!  otherwise: open the tunnel, try the likely addresses inside it
//!     one answers ─────────▶ pin the key it proves; it goes in [hosts]
//!     none, or no handshake ▶ say why, save nothing
//!  save the profile and the key; make it the default if there's none
//! ```
//!
//! Nothing is saved until the setup is known to work, or you've said you're
//! home, so running it again after fixing something starts clean.
//!
//! Pinning through the tunnel needs no trust: WireGuard has authenticated
//! the server. Pinning at home trusts the home network for that one moment,
//! and `gitaway check` from another network confirms it later.

use std::io::{BufRead, Read};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use zeroize::Zeroizing;

use crate::address::HostPort;
use crate::config::name::ProfileName;
use crate::config::profile::{HostMap, Profile, Transport};
use crate::config::store::Store;
use crate::config::value::HostKey;
use crate::error::Error;
use crate::git::remote_url::{self, RemoteUrl, Scheme};
use crate::home_check::{self, Timeouts};
use crate::keys::device_key::DeviceKey;
use crate::keys::store::KeyStore;
use crate::transport::client_config::{self, ClientConfig};
use crate::transport::{self as tunnel, TransportError};

use super::args::SetupArgs;
use super::check_cmd::{CAPTURE_TIMEOUT, algorithm_name};
use super::key_cmd::{IMPORT_LIMIT, parse_private_key, warn_if_fallback};
use super::profile_cmd::{NewProfile, build_profile, set_default};
use super::{away, prompt};

/// What gitaway learned about the git server.
struct Found {
    /// Its address inside the tunnel.
    inside: IpAddr,
    /// The host key it proved, when it has SSH.
    host_key: Option<HostKey>,
}

/// Runs `gitaway setup`.
pub fn run(store: &Store, args: SetupArgs) -> Result<i32, Error> {
    let keys = KeyStore::from_env(store.dir().clone())?;
    let name = prompt::with_default(
        args.name,
        "the profile name",
        "Name for this profile",
        "home",
        |s| {
            let name: ProfileName = s.parse().map_err(|e| format!("{e}"))?;
            if store.profile_exists(&name) {
                return Err(format!(
                    "a profile named `{name}` already exists; pick another name, or remove it \
                     with `gitaway profile remove {name}`"
                ));
            }
            Ok(name)
        },
    )?;

    let (mut config, source) = read_config(args.config)?;
    let key = match config.key.take() {
        Some(key) => key,
        None => ask_private_key(&config)?,
    };
    let (url, url_text) = prompt::required(
        args.url,
        "--url",
        "A clone URL from your git server, as you use it at home \
         (such as ssh://git@192.0.2.10:2222/alice/app.git)",
        |s| parse_url(s).map(|url| (url, s.trim().to_owned())),
    )?;
    let ssh_port = match url.scheme {
        Scheme::Ssh => Some(url.effective_port()),
        _ => prompt::optional(
            args.ssh_port,
            "--ssh-port",
            "Your git server's SSH port, which gitaway uses to recognise home",
            |s| {
                s.trim()
                    .parse::<u16>()
                    .map_err(|_| "give a port number, such as 22".to_owned())
            },
        )?,
    };
    let ssh = ssh_port.map(|port| HostPort {
        host: url.host.clone(),
        port: Some(port),
    });
    let likely = match args.inside {
        Some(inside) => vec![
            inside
                .trim()
                .parse::<IpAddr>()
                .map_err(|_| Error::msg("--inside: give an IP address, such as 10.0.0.1"))?,
        ],
        None => config.likely_addresses(&url.host),
    };
    if likely.is_empty() {
        return Err(Error::msg(format!(
            "the config doesn't show where {} is inside the tunnel. Run setup again with \
             --inside <its address inside the tunnel>",
            url.host
        )));
    }

    let found = find_git_server(&name, &config, &key, &url, ssh.as_ref(), &likely, args.yes)?;

    let new = NewProfile {
        transport: config.transport,
        home: ssh.map(|ssh| (ssh, found.host_key)),
        hosts: vec![(
            HostPort {
                host: url.host.clone(),
                port: None,
            },
            HostPort {
                host: found.inside.to_string(),
                port: None,
            },
        )],
    };
    store.create_profile(&name, &build_profile(&new).to_string())?;
    if let Err(e) = keys.save(&name, &key) {
        // Without its key the profile can't be used, so don't leave it half made.
        store.remove_profile(&name)?;
        return Err(e.into());
    }
    println!(
        "\nSaved profile `{name}` ({}), with this device's key in {}.",
        store.dir().profile_file(&name).display(),
        keys.location(&name)
    );
    warn_if_fallback(&keys, &name);
    if store.load_global()?.default_profile.is_none() {
        set_default(store, &name)?;
    }
    match source {
        Source::File(file) => println!(
            "{} holds this device's private key, which gitaway has now stored, \
             so you can delete the file.",
            file.display()
        ),
        Source::Pasted => println!(
            "The config you pasted, with its private key, is still on your screen: clear the \
             terminal when you're done (Cmd+K on macOS, or `clear`)."
        ),
        Source::Stdin => {}
    }
    if new.home.is_none() {
        println!(
            "Without an SSH port gitaway can't tell when you're home, so it will always use \
             the tunnel for this profile."
        );
    }
    println!(
        "\nDone. Try it: git away ls-remote {url_text}\n\
         Clone with `git away clone <url>`. In a repository you cloned before, run \
         `gitaway use {name}` once."
    );
    Ok(0)
}

/// Finds the git server: directly, if it answers on this network and you
/// say you're home; otherwise through the tunnel.
fn find_git_server(
    name: &ProfileName,
    config: &ClientConfig,
    key: &DeviceKey,
    url: &RemoteUrl,
    ssh: Option<&HostPort>,
    likely: &[IpAddr],
    yes: bool,
) -> Result<Found, Error> {
    let runtime = away::runtime()?;
    let seen_here = match ssh {
        Some(ssh) => runtime.block_on(look_directly(ssh)),
        None => None,
    };
    let result = match (ssh, seen_here) {
        (Some(ssh), Some(seen)) if at_home(ssh, &seen, yes)? => {
            println!(
                "gitaway will use its host key to recognise home.\n\
                 Tunnel: not tested. From inside your home network it would need your router to \
                 reach its own public address, which many can't. Run `gitaway check {name}` once \
                 from another network (a phone's hotspot) to test it.\n\
                 Inside the tunnel, gitaway will look for {} at {} (from the config's routes).",
                url.host, likely[0]
            );
            Ok(Found {
                inside: likely[0],
                host_key: Some(seen),
            })
        }
        (_, seen_here) => runtime.block_on(search_tunnel(
            name,
            config,
            key.duplicate(),
            url,
            ssh,
            likely,
            seen_here.as_ref(),
        )),
    };
    runtime.shutdown_timeout(away::SHUTDOWN_GRACE);
    result
}

/// Whether the user is home, asked after the git server answered directly.
fn at_home(ssh: &HostPort, seen: &HostKey, yes: bool) -> Result<bool, Error> {
    println!(
        "An SSH server answered at {ssh} on this network ({} {}).",
        algorithm_name(seen),
        seen.fingerprint()
    );
    Ok(yes
        || (prompt::interactive() && prompt::confirm("Are you on your home network right now?")?))
}

/// The host key of an SSH server on this network, if one answers quickly.
async fn look_directly(ssh: &HostPort) -> Option<HostKey> {
    let timeouts = Timeouts::default();
    let address = (ssh.host.as_str(), ssh.port.unwrap_or(22));
    let connecting = tokio::net::TcpStream::connect(address);
    let stream = tokio::time::timeout(timeouts.connect, connecting)
        .await
        .ok()?
        .ok()?;
    home_check::capture(stream, timeouts.exchange, None)
        .await
        .ok()
}

/// Opens the tunnel and tries each likely address of the git server.
async fn search_tunnel(
    name: &ProfileName,
    config: &ClientConfig,
    key: DeviceKey,
    url: &RemoteUrl,
    ssh: Option<&HostPort>,
    likely: &[IpAddr],
    seen_here: Option<&HostKey>,
) -> Result<Found, Error> {
    let Transport::WireGuard(wg) = &config.transport;
    println!("Connecting to your VPN server at {}…", wg.endpoint);
    let profile = Profile {
        transport: config.transport.clone(),
        home: None,
        hosts: HostMap::default(),
    };
    let transport = tunnel::open(name.as_str(), &profile, key).await?;
    let port = ssh.and_then(|ssh| ssh.port).unwrap_or(url.effective_port());

    let mut tried = Vec::new();
    for &inside in likely {
        let target = SocketAddr::new(inside, port);
        println!("Looking for your git server inside the tunnel at {target}…");
        let stream = match transport.connect(target).await {
            Ok(stream) => stream,
            Err(TransportError::NoHandshake { endpoint }) => {
                return Err(Error::msg(format!(
                    "no answer from your VPN server at {endpoint}. Check that the server has \
                     added this device (the config came from it, so the keys match once it \
                     has), that the config's Endpoint is right, and that UDP port {} reaches \
                     the server from here.\nIf you're on your home network, your router may \
                     not be able to reach its own public address: run setup from another \
                     network, such as a phone's hotspot.",
                    endpoint.port()
                )));
            }
            Err(TransportError::Timeout { .. }) => {
                tried.push(format!("{target}: no answer"));
                continue;
            }
            Err(TransportError::Connect { source, .. }) => {
                tried.push(format!("{target}: {source}"));
                continue;
            }
            Err(other) => return Err(other.into()),
        };
        println!("Tunnel: works.");
        if ssh.is_none() {
            println!("Found your git server at {target}.");
            return Ok(Found {
                inside,
                host_key: None,
            });
        }
        match home_check::capture(stream, CAPTURE_TIMEOUT, None).await {
            Ok(key) => {
                println!(
                    "Found your git server at {target}. Its host key: {} {}",
                    algorithm_name(&key),
                    key.fingerprint()
                );
                report_seen_here(seen_here, &key);
                println!("gitaway will use it to recognise home.");
                return Ok(Found {
                    inside,
                    host_key: Some(key),
                });
            }
            Err(e) => tried.push(format!("{target}: answered, but {e}")),
        }
    }
    Err(Error::msg(format!(
        "the tunnel works, but your git server didn't answer inside it:\n  {}\n\
         Those addresses are guesses from the config's routes. If you know the git server's \
         address inside the tunnel, run setup again with --inside <address>.",
        tried.join("\n  ")
    )))
}

/// Compares the key found through the tunnel with whatever answered on
/// this network at the same address.
fn report_seen_here(seen_here: Option<&HostKey>, through_tunnel: &HostKey) {
    match seen_here {
        Some(seen) if seen.same_key(through_tunnel) => println!(
            "That's the same server that answered on this network, so you are home: \
             here, commands will connect to it directly."
        ),
        Some(_) => println!(
            "The server that answered on this network has a different key, so it's another \
             machine: here, commands will use the tunnel."
        ),
        None => {}
    }
}

/// Where the VPN config came from, for the closing advice.
enum Source {
    File(PathBuf),
    Pasted,
    Stdin,
}

/// Reads the VPN config: the file named by `--config` (`-` for stdin), or,
/// in a terminal, a path or the pasted config itself.
fn read_config(flag: Option<String>) -> Result<(ClientConfig, Source), Error> {
    match flag {
        Some(flag) if flag == "-" => {
            let mut text = Zeroizing::new(String::new());
            std::io::stdin()
                .take(IMPORT_LIMIT + 1)
                .read_to_string(&mut text)
                .map_err(|e| Error::msg(format!("couldn't read the config from stdin: {e}")))?;
            Ok((parse_config(&text)?, Source::Stdin))
        }
        // Never echo it: it holds the private key.
        Some(flag) if looks_like_config_text(&flag) => Err(Error::msg(
            "--config takes the path of a config file, not the config itself. On the command \
             line, its private key ends up in your shell's history. Save the config in a file \
             and give its path, or run `gitaway setup` without --config and paste it when asked",
        )),
        Some(flag) => {
            let path = clean_path(&flag);
            let text = read_file(&path).map_err(|e| Error::msg(format!("--config: {e}")))?;
            Ok((parse_config(&text)?, Source::File(path)))
        }
        None if !prompt::interactive() => Err(Error::msg(
            "--config is required when not running in a terminal",
        )),
        None => ask_config(),
    }
}

/// Asks for the config's path, or for the config itself, pasted. Asks
/// again until it's a config gitaway can use.
///
/// Read line by line from the terminal rather than with a prompt library,
/// so a pasted config arrives whole instead of spilling into the next
/// questions.
fn ask_config() -> Result<(ClientConfig, Source), Error> {
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        eprint!(
            "The VPN config your server gave this device: type the file's path, or paste \
             the config: "
        );
        let Some(first) = lines.next().transpose().map_err(read_error)? else {
            return Err(Error::msg("no config given"));
        };
        let first = Zeroizing::new(first);
        let attempt = if looks_like_config_text(&first) {
            read_pasted(&first, &mut lines, stdin_idle)
                .and_then(|text| parse_config(&text).map_err(|e| e.to_string()))
                .map(|config| (config, Source::Pasted))
        } else if first.trim().is_empty() {
            continue;
        } else {
            let path = clean_path(&first);
            read_file(&path)
                .and_then(|text| parse_config(&text).map_err(|e| e.to_string()))
                .map(|config| (config, Source::File(path)))
        };
        match attempt {
            Ok(found) => return Ok(found),
            Err(e) => eprintln!("{e}"),
        }
    }
}

/// The rest of a pasted config, after its `first` line.
///
/// A paste usually ends without a line break, so its last line waits for
/// Enter. Once what's arrived is a whole config and nothing more is coming
/// (`idle`), this says "Press Enter to finish", and the next line ends it:
/// an empty one, or the last pasted line that the Enter completed. Without
/// that (no way to tell `idle`), an empty line after a whole config ends
/// it. Two empty lines in a row, or the end of input, always do.
fn read_pasted(
    first: &str,
    lines: &mut impl Iterator<Item = std::io::Result<String>>,
    mut idle: impl FnMut() -> bool,
) -> Result<Zeroizing<String>, String> {
    let mut text = Zeroizing::new(format!("{first}\n"));
    let mut blank_before = false;
    let mut asked_to_finish = false;
    loop {
        let whole = client_config::read(&text).is_ok();
        if whole && !asked_to_finish && idle() {
            eprintln!("\n(Press Enter to finish.)");
            asked_to_finish = true;
        }
        let Some(line) = lines.next() else { break };
        let line = Zeroizing::new(line.map_err(|e| read_error(e).to_string())?);
        let blank = line.trim().is_empty();
        if blank && (blank_before || whole) {
            break;
        }
        text.push_str(&line);
        text.push('\n');
        if text.len() as u64 > IMPORT_LIMIT {
            return Err("that's too long to be a VPN config".to_owned());
        }
        if asked_to_finish {
            break;
        }
        blank_before = blank;
    }
    Ok(text)
}

/// Whether no more pasted lines are arriving: none is ready within a
/// moment. A terminal hands over whole lines only, so an unfinished last
/// line doesn't count.
#[cfg(unix)]
fn stdin_idle() -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    let stdin = std::io::stdin();
    let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
    let moment = Timespec {
        tv_sec: 0,
        tv_nsec: 150_000_000,
    };
    matches!(poll(&mut fds, Some(&moment)), Ok(0))
}

/// Elsewhere there's no cheap way to tell, so an empty line ends a paste.
#[cfg(not(unix))]
fn stdin_idle() -> bool {
    false
}

/// Whether text is a config itself rather than a path to one.
fn looks_like_config_text(text: &str) -> bool {
    text.contains('\n') || text.trim_start().starts_with('[')
}

fn read_error(e: std::io::Error) -> Error {
    Error::msg(format!("couldn't read your answer: {e}"))
}

fn parse_config(text: &str) -> Result<ClientConfig, Error> {
    if text.len() as u64 > IMPORT_LIMIT {
        return Err(Error::msg("that's too long to be a VPN config"));
    }
    client_config::read(text).map_err(|e| Error::msg(e.to_string()))
}

/// Reads a config file, refusing anything far bigger than a config.
fn read_file(path: &PathBuf) -> Result<Zeroizing<String>, String> {
    let file =
        std::fs::File::open(path).map_err(|e| format!("couldn't open {}: {e}", path.display()))?;
    let mut text = Zeroizing::new(String::new());
    file.take(IMPORT_LIMIT + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
    Ok(text)
}

/// A path as typed, or as a file dragged into the terminal gives it:
/// quoted, or with spaces escaped by backslashes. A leading `~/` means the
/// home directory.
fn clean_path(typed: &str) -> PathBuf {
    let typed = typed.trim();
    let unquoted = ['\'', '"']
        .iter()
        .find_map(|&q| typed.strip_prefix(q).and_then(|t| t.strip_suffix(q)))
        .map(str::to_owned)
        .unwrap_or_else(|| typed.replace("\\ ", " "));
    match (unquoted.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(unquoted),
    }
}

/// Asks for the device's private key when the config doesn't include it.
fn ask_private_key(config: &ClientConfig) -> Result<DeviceKey, Error> {
    if !prompt::interactive() {
        return Err(Error::msg(
            "the config has no PrivateKey. Use one that includes this device's private key, \
             or run setup in a terminal to paste the key",
        ));
    }
    println!("The config has no PrivateKey, so this device's key was made separately.");
    let text = prompt::secret("Paste this device's private key (input hidden)")?;
    let key = parse_private_key(&text).map_err(Error::msg)?;
    let Transport::WireGuard(wg) = &config.transport;
    if key.public_key() == wg.server_public_key {
        return Err(Error::msg(
            "that's the VPN server's own private key, which must never leave the server",
        ));
    }
    Ok(key)
}

fn parse_url(text: &str) -> Result<RemoteUrl, String> {
    let url = remote_url::parse(text.trim()).ok_or_else(|| {
        "that isn't a git URL that goes over the network, such as \
         ssh://git@192.0.2.10:2222/alice/app.git"
            .to_owned()
    })?;
    if url.scheme == Scheme::Git {
        return Err("git:// URLs aren't supported; use the ssh:// or http(s):// URL".to_owned());
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragged_quoted_and_home_relative_paths_are_cleaned() {
        assert_eq!(clean_path("  laptop.conf \n"), PathBuf::from("laptop.conf"));
        assert_eq!(
            clean_path("'/tmp/my vpn.conf'"),
            PathBuf::from("/tmp/my vpn.conf")
        );
        assert_eq!(
            clean_path("\"/tmp/my vpn.conf\""),
            PathBuf::from("/tmp/my vpn.conf")
        );
        assert_eq!(
            clean_path("/tmp/my\\ vpn.conf"),
            PathBuf::from("/tmp/my vpn.conf")
        );
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(
                clean_path("~/vpn.conf"),
                PathBuf::from(home).join("vpn.conf")
            );
        }
    }

    /// Reads a paste of `lines`, with `idle` saying whether more is coming.
    fn pasted_with(lines: &[&str], idle: bool) -> Result<String, String> {
        let mut rest = lines[1..].iter().map(|line| Ok(line.to_string()));
        let text = read_pasted(lines[0], &mut rest, || idle)?;
        let left: Vec<String> = rest.map(Result::unwrap).collect();
        Ok(format!("{}--left: {}", text.as_str(), left.join("|")))
    }

    fn pasted(lines: &[&str]) -> Result<String, String> {
        pasted_with(lines, false)
    }

    const CONFIG: &[&str] = &[
        "[Interface]",
        "PrivateKey = dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo=",
        "Address = 10.0.0.2/32",
        "",
        "[Peer]",
        "PublicKey = 3p7bfXt9wbTTW2HC7OQ1Nz+DQ8hbeGdNrfx+FG+IK08=",
        "Endpoint = vpn.example.net:51820",
        "AllowedIPs = 10.0.0.1/32",
    ];

    #[test]
    fn a_pasted_config_ends_at_the_first_empty_line_after_it_is_whole() {
        let mut lines = CONFIG.to_vec();
        lines.extend(["", "next answer"]);
        let text = pasted(&lines).unwrap();
        // The empty line between the sections didn't end it, and the line
        // after the config was left for the next question.
        assert!(
            text.contains("[Peer]") && text.contains("AllowedIPs"),
            "{text}"
        );
        assert!(text.ends_with("--left: next answer"), "{text}");
        assert!(client_config::read(text.split("--left").next().unwrap()).is_ok());
    }

    #[test]
    fn a_pasted_config_that_never_becomes_whole_ends_at_two_empty_lines_or_the_end() {
        let broken = [
            "[Interface]",
            "Address = 10.0.0.2/32",
            "",
            "",
            "next answer",
        ];
        assert!(pasted(&broken).unwrap().ends_with("--left: next answer"));
        assert!(pasted(&CONFIG[..3]).unwrap().ends_with("--left: "));
    }

    #[test]
    fn once_a_whole_config_sits_idle_one_enter_ends_the_paste() {
        // The paste's last line had no line break: the Enter completes it,
        // and it's kept.
        let mut unfinished = CONFIG.to_vec();
        unfinished.extend(["PersistentKeepalive = 25", "next answer"]);
        let text = pasted_with(&unfinished, true).unwrap();
        assert!(
            text.contains("PersistentKeepalive = 25\n--left: next answer"),
            "{text}"
        );

        // The paste ended with a line break: the Enter gives an empty line.
        let mut finished = CONFIG.to_vec();
        finished.extend(["", "next answer"]);
        let text = pasted_with(&finished, true).unwrap();
        assert!(
            text.ends_with("AllowedIPs = 10.0.0.1/32\n--left: next answer"),
            "{text}"
        );
    }

    #[test]
    fn config_text_is_told_apart_from_a_path() {
        assert!(looks_like_config_text("[Interface]"));
        assert!(looks_like_config_text("  [Interface]\nPrivateKey = x"));
        assert!(!looks_like_config_text("laptop.conf"));
        assert!(!looks_like_config_text("/tmp/my vpn.conf"));
    }

    #[test]
    fn urls_must_reach_a_network_server_over_ssh_or_http() {
        assert!(parse_url("ssh://git@192.0.2.10:2222/alice/app.git").is_ok());
        assert!(parse_url("git@192.0.2.10:alice/app.git").is_ok());
        assert!(parse_url("https://git.example.com/alice/app.git").is_ok());
        assert!(parse_url("/srv/git/app.git").is_err());
        assert!(
            parse_url("git://192.0.2.10/app.git")
                .unwrap_err()
                .contains("ssh://")
        );
    }
}
