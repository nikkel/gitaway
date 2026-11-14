//! The note a running `gitaway tunnel` leaves for other gitaway commands, so
//! they can use its tunnel instead of waiting for it to stop.
//!
//! ```text
//!  gitaway tunnel                          gitaway push (same profile)
//!  holds the profile's lock                lock busy ─▶ read runs/<profile>.toml
//!  proxy on 127.0.0.1:<random>  ◀────────  connect with the token, ask for an
//!  writes runs/<profile>.toml              address inside the tunnel
//!  (address + token, mode 600)
//!  on exit: removes it
//! ```
//!
//! The file holds the address of the tunnel's proxy for other gitaway
//! processes, the random token that proxy asks for, and the profile's VPN
//! settings as the tunnel started with them (no secrets), so a command can
//! tell when the profile has changed since. It's private (mode
//! 600 in gitaway's private configuration directory), like the device key
//! file: a program that can read it could read the device key too.
//!
//! A `kill -9` or a crash leaves the file behind, but it also releases the
//! profile's lock. So a file is only trusted while the lock is held: a
//! command that finds the lock free deletes any file it finds as stale.

use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use super::name::ProfileName;
use super::paths::ConfigDir;
use super::private_fs::write_atomic;

/// Where a running tunnel's proxy for other gitaway processes is, the token
/// it asks for, and the VPN settings the tunnel was opened with.
#[derive(Clone)]
pub struct RunFile {
    proxy: SocketAddr,
    token: Zeroizing<String>,
    transport: String,
}

/// The longest VPN settings line accepted from the file.
const MAX_TRANSPORT_TEXT: usize = 1024;

/// The runs file couldn't be written, read or removed, or isn't valid.
#[derive(Debug, thiserror::Error)]
#[error("{}: {reason}", path.display())]
pub struct RunFileError {
    path: PathBuf,
    reason: String,
}

/// The file's contents, before they're checked.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    proxy: String,
    token: String,
    transport: String,
}

impl RunFile {
    /// A runs file for the proxy at `proxy` (on loopback) asking for `token`,
    /// for a tunnel opened with `transport`
    /// ([`crate::config::profile::Transport::settings_text`]).
    pub fn new(proxy: SocketAddr, token: &str, transport: &str) -> Self {
        Self {
            proxy,
            token: Zeroizing::new(token.to_owned()),
            transport: transport.to_owned(),
        }
    }

    /// Where the proxy listens.
    pub fn proxy(&self) -> SocketAddr {
        self.proxy
    }

    /// The token the proxy asks for.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The VPN settings the tunnel was opened with.
    pub fn transport(&self) -> &str {
        &self.transport
    }

    /// Writes the file for profile `name`. It's removed when the returned
    /// [`Published`] is dropped.
    pub fn publish(&self, dir: &ConfigDir, name: &ProfileName) -> Result<Published, RunFileError> {
        let path = dir.run_file(name);
        let text = Zeroizing::new(format!(
            "# Written by a running `gitaway tunnel`, and removed when it stops.\n\
             proxy = \"{}\"\ntoken = \"{}\"\ntransport = {}\n",
            self.proxy,
            self.token.as_str(),
            // Quoted and escaped as a TOML string.
            toml::Value::String(self.transport.clone())
        ));
        write_atomic(dir.path(), &path, text.as_bytes()).map_err(|e| error(&path, e))?;
        Ok(Published { path })
    }

    /// Reads profile `name`'s file: `None` if there isn't one. Only meaningful
    /// while another process holds the profile's lock.
    pub fn read(dir: &ConfigDir, name: &ProfileName) -> Result<Option<Self>, RunFileError> {
        let path = dir.run_file(name);
        let text = match fs::read_to_string(&path) {
            Ok(text) => Zeroizing::new(text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(error(&path, e)),
        };
        Self::parse(&text).map(Some).map_err(|reason| RunFileError {
            path: path.clone(),
            reason,
        })
    }

    /// Deletes profile `name`'s file, if there is one: call this holding the
    /// profile's lock, when any file there was left by a tunnel that didn't
    /// stop cleanly.
    pub fn remove_stale(dir: &ConfigDir, name: &ProfileName) -> Result<(), RunFileError> {
        let path = dir.run_file(name);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(error(&path, e)),
        }
    }

    fn parse(text: &str) -> Result<Self, String> {
        let raw: Raw = toml::from_str(text).map_err(|e| e.message().to_owned())?;
        let token = Zeroizing::new(raw.token);
        let proxy: SocketAddr = raw
            .proxy
            .parse()
            .map_err(|_| format!("`{}` isn't an address and port", raw.proxy))?;
        // gitaway only ever listens on loopback; anything else is a file
        // gitaway didn't write, and the token mustn't be sent there.
        if !proxy.ip().is_loopback() {
            return Err(format!("{proxy} isn't on this computer"));
        }
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("the token isn't 64 hex digits".to_owned());
        }
        if raw.transport.is_empty() || raw.transport.len() > MAX_TRANSPORT_TEXT {
            return Err("the VPN settings are empty or too long".to_owned());
        }
        Ok(Self {
            proxy,
            token,
            transport: raw.transport,
        })
    }
}

/// Never prints the token.
impl std::fmt::Debug for RunFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunFile")
            .field("proxy", &self.proxy)
            .finish_non_exhaustive()
    }
}

/// A written runs file. Dropping it removes the file.
#[derive(Debug)]
pub struct Published {
    path: PathBuf,
}

impl Drop for Published {
    fn drop(&mut self) {
        // Nothing more can be done if it fails: the next command finds the
        // lock free and deletes the file as stale.
        let _ = fs::remove_file(&self.path);
    }
}

fn error(path: &Path, e: io::Error) -> RunFileError {
    RunFileError {
        path: path.to_owned(),
        reason: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    /// With a quote and a backslash, which must survive the round trip.
    const SETTINGS: &str = r#"wireguard endpoint=vpn.example.net:51820 "x\y""#;

    fn dir(temp: &TempDir) -> ConfigDir {
        ConfigDir::new(Some(temp.path().into()), None).unwrap()
    }

    fn name() -> ProfileName {
        "home".parse().unwrap()
    }

    #[test]
    fn a_published_file_reads_back_and_is_removed_when_dropped() {
        let temp = TempDir::new().unwrap();
        let proxy: SocketAddr = "127.0.0.1:41234".parse().unwrap();
        let published = RunFile::new(proxy, TOKEN, SETTINGS)
            .publish(&dir(&temp), &name())
            .unwrap();

        let read = RunFile::read(&dir(&temp), &name()).unwrap().unwrap();
        assert_eq!(read.proxy(), proxy);
        assert_eq!(read.token(), TOKEN);
        assert_eq!(read.transport(), SETTINGS);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dir(&temp).run_file(&name()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        drop(published);
        assert!(RunFile::read(&dir(&temp), &name()).unwrap().is_none());
    }

    #[test]
    fn a_stale_file_is_removed_and_a_missing_one_is_fine() {
        let temp = TempDir::new().unwrap();
        let published = RunFile::new("127.0.0.1:41234".parse().unwrap(), TOKEN, SETTINGS)
            .publish(&dir(&temp), &name())
            .unwrap();
        // As after a `kill -9`: the file stays.
        std::mem::forget(published);
        RunFile::remove_stale(&dir(&temp), &name()).unwrap();
        assert!(RunFile::read(&dir(&temp), &name()).unwrap().is_none());
        RunFile::remove_stale(&dir(&temp), &name()).unwrap();
    }

    #[test]
    fn files_gitaway_did_not_write_are_refused() {
        let cases = [
            (
                "not on this computer",
                format!("proxy = \"192.0.2.10:1080\"\ntoken = \"{TOKEN}\"\ntransport = \"t\"\n"),
            ),
            (
                "short token",
                "proxy = \"127.0.0.1:1080\"\ntoken = \"abc\"\n".to_owned(),
            ),
            (
                "unknown field",
                format!(
                    "proxy = \"127.0.0.1:1080\"\ntoken = \"{TOKEN}\"\ntransport = \"t\"\n\
                     host = \"x\"\n"
                ),
            ),
            (
                "no port",
                format!("proxy = \"127.0.0.1\"\ntoken = \"{TOKEN}\"\n"),
            ),
            ("not TOML", "proxy =".to_owned()),
        ];
        for (what, text) in cases {
            assert!(RunFile::parse(&text).is_err(), "{what}");
        }
        let v6 = format!("proxy = \"[::1]:1080\"\ntoken = \"{TOKEN}\"\ntransport = \"t\"\n");
        assert!(RunFile::parse(&v6).is_ok());
    }

    #[test]
    fn debug_output_leaves_the_token_out() {
        let run = RunFile::new("127.0.0.1:41234".parse().unwrap(), TOKEN, SETTINGS);
        assert!(!format!("{run:?}").contains(TOKEN));
    }
}
