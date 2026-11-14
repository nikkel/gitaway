//! Getting a profile's tunnel for a command: open one, or borrow the one a
//! running `gitaway tunnel` has open.
//!
//! The server keeps one session per device key, so only one process at a
//! time may open the profile's tunnel: the one holding the profile's lock
//! (see [`crate::config::lock`]).
//!
//! ```text
//!  lock free? ─── yes ─▶ delete any runs file (left by a tunnel that was killed)
//!   │                    load the device key, open this command's own tunnel
//!   no
//!  runs file, and its proxy answers? ─── yes ─▶ same VPN settings as the profile now?
//!   │                                             yes ─▶ borrow the running tunnel
//!   │                                             no ──▶ error: restart `gitaway tunnel`
//!   no: a short command holds the lock, or a tunnel is starting
//!  say "waiting" once, look again in 200 ms
//! ```
//!
//! `gitaway tunnel` itself never borrows: a second one for the same profile
//! is an error, since stopping the first would break it.

use std::net::TcpStream;
use std::time::Duration;

use crate::config::lock::ProfileLock;
use crate::config::name::ProfileName;
use crate::config::paths::ConfigDir;
use crate::config::profile::Profile;
use crate::config::run_file::RunFile;
use crate::config::store::Store;
use crate::error::Error;
use crate::keys::store::{DeviceSecrets, KeyStore};
use crate::transport::shared::SharedTunnel;
use crate::transport::{self, Lifetime, Transport, TransportError};

/// How often to look again while another command holds the lock.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// How long a running tunnel's proxy may take to accept a connection.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// What a command found when it looked for the profile's tunnel.
#[derive(Debug)]
pub(crate) enum Access {
    /// The lock was free, and is now held by this process, which opens the
    /// tunnel itself. Any runs file has been deleted as stale.
    Free(ProfileLock),
    /// A `gitaway tunnel` is running for the profile and lends its tunnel.
    Running(RunFile),
}

/// Where a command's tunnel comes from.
pub(crate) enum Source {
    /// Its own, opened with this device's secrets.
    Own(DeviceSecrets),
    /// A running `gitaway tunnel`'s.
    Borrowed(RunFile),
}

impl Source {
    /// Opens the command's own tunnel, or connects to the running one.
    pub(crate) async fn open(
        self,
        name: &ProfileName,
        profile: &Profile,
    ) -> Result<Box<dyn Transport>, TransportError> {
        match self {
            Self::Own(secrets) => {
                transport::open(name.as_str(), profile, secrets, Lifetime::OneCommand).await
            }
            Self::Borrowed(run) => Ok(Box::new(SharedTunnel::new(run))),
        }
    }
}

/// Gets the profile's tunnel for a command such as a push: borrows a
/// running `gitaway tunnel`'s, or takes the lock and loads the device key to
/// open its own, waiting while another command uses it. Hold the lock, if
/// any, until the tunnel is closed.
///
/// A running tunnel opened with VPN settings other than `profile`'s (the
/// profile changed since it started) is an error: it may lead to another
/// server.
pub(crate) fn for_command(
    store: &Store,
    name: &ProfileName,
    profile: &Profile,
) -> Result<(Option<ProfileLock>, Source), Error> {
    match wait_for(store.dir(), name, || note_waiting(name))? {
        Access::Free(lock) => {
            let secrets = load_secrets(store, name)?;
            Ok((Some(lock), Source::Own(secrets)))
        }
        Access::Running(run) if run.transport() != profile.transport.settings_text() => {
            Err(Error::msg(format!(
                "the running `gitaway tunnel` for profile `{name}` was started before the \
                 profile's VPN settings changed, so it may lead somewhere else. Restart it: stop \
                 it with Ctrl+C and run `gitaway tunnel {name}` again"
            )))
        }
        Access::Running(run) => {
            eprintln!("gitaway: using the running `gitaway tunnel` for profile `{name}`.");
            Ok((None, Source::Borrowed(run)))
        }
    }
}

/// Takes the lock and loads the device key for `gitaway tunnel`, waiting
/// while a command uses the tunnel. Another `gitaway tunnel` already
/// running for the profile is an error.
pub(crate) fn for_tunnel(
    store: &Store,
    name: &ProfileName,
) -> Result<(ProfileLock, DeviceSecrets), Error> {
    match wait_for(store.dir(), name, || note_waiting(name))? {
        Access::Free(lock) => Ok((lock, load_secrets(store, name)?)),
        Access::Running(_) => Err(Error::msg(format!(
            "`gitaway tunnel` is already running for profile `{name}`. Use that one, or stop \
             it first"
        ))),
    }
}

/// Waits until the profile's lock is free (and takes it) or a running
/// `gitaway tunnel` can lend its tunnel. `on_wait` runs once, before
/// waiting.
pub(crate) fn wait_for(
    dir: &ConfigDir,
    name: &ProfileName,
    on_wait: impl FnOnce(),
) -> Result<Access, Error> {
    let mut on_wait = Some(on_wait);
    loop {
        if let Some(lock) =
            ProfileLock::try_acquire(dir, name).map_err(|e| Error::msg(e.to_string()))?
        {
            RunFile::remove_stale(dir, name).map_err(|e| Error::msg(e.to_string()))?;
            return Ok(Access::Free(lock));
        }
        if let Some(run) = RunFile::read(dir, name).map_err(|e| Error::msg(e.to_string()))?
            && TcpStream::connect_timeout(&run.proxy(), PROBE_TIMEOUT).is_ok()
        {
            return Ok(Access::Running(run));
        }
        if let Some(on_wait) = on_wait.take() {
            on_wait();
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn note_waiting(name: &ProfileName) {
    eprintln!(
        "gitaway: another gitaway command is using profile `{name}`'s tunnel; waiting for it to \
         finish…"
    );
}

fn load_secrets(store: &Store, name: &ProfileName) -> Result<DeviceSecrets, Error> {
    let keys = KeyStore::from_env(store.dir().clone())?;
    keys.load(name)?.ok_or_else(|| {
        Error::msg(format!(
            "profile `{name}` has no device key, so it can't open the tunnel. \
             Create one with `gitaway key rotate {name}` and give the server its public key"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;
    use tempfile::TempDir;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn dir(temp: &TempDir) -> ConfigDir {
        ConfigDir::new(Some(temp.path().into()), None).unwrap()
    }

    fn name() -> ProfileName {
        "home".parse().unwrap()
    }

    /// Holds the profile's lock, as another process would.
    fn hold_lock(temp: &TempDir) -> ProfileLock {
        ProfileLock::try_acquire(&dir(temp), &name())
            .unwrap()
            .unwrap()
    }

    #[test]
    fn a_free_lock_is_taken_and_a_stale_runs_file_deleted() {
        let temp = TempDir::new().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        // Left by a tunnel that was killed; its proxy even still answers.
        std::mem::forget(
            RunFile::new(listener.local_addr().unwrap(), TOKEN, "wireguard")
                .publish(&dir(&temp), &name())
                .unwrap(),
        );
        let access = wait_for(&dir(&temp), &name(), || panic!("nothing to wait for")).unwrap();
        assert!(matches!(access, Access::Free(_)), "{access:?}");
        assert!(RunFile::read(&dir(&temp), &name()).unwrap().is_none());
        assert!(
            ProfileLock::try_acquire(&dir(&temp), &name())
                .unwrap()
                .is_none(),
            "the lock is held"
        );
    }

    #[test]
    fn a_running_tunnel_is_borrowed_without_waiting() {
        let temp = TempDir::new().unwrap();
        let _lock = hold_lock(&temp);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy = listener.local_addr().unwrap();
        let _published = RunFile::new(proxy, TOKEN, "wireguard")
            .publish(&dir(&temp), &name())
            .unwrap();
        let access = wait_for(&dir(&temp), &name(), || panic!("nothing to wait for")).unwrap();
        assert!(
            matches!(&access, Access::Running(run) if run.proxy() == proxy),
            "{access:?}"
        );
    }

    fn profile(mtu: &str) -> Profile {
        Profile::from_toml(&format!(
            "[transport.wireguard]\nendpoint = \"vpn.example.net:51820\"\n\
             server_public_key = \"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"\n{mtu}"
        ))
        .unwrap()
    }

    #[test]
    fn a_running_tunnel_with_other_vpn_settings_is_not_borrowed() {
        let temp = TempDir::new().unwrap();
        let store = Store::new(dir(&temp));
        let _lock = hold_lock(&temp);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let started_with = profile("");
        let _published = RunFile::new(
            listener.local_addr().unwrap(),
            TOKEN,
            &started_with.transport.settings_text(),
        )
        .publish(&dir(&temp), &name())
        .unwrap();

        let (lock, source) = for_command(&store, &name(), &started_with).unwrap();
        assert!(lock.is_none());
        assert!(matches!(source, Source::Borrowed(_)));

        let changed = profile("mtu = 1280\n");
        let error = for_command(&store, &name(), &changed).err().unwrap();
        assert!(error.to_string().contains("Restart it"), "{error}");
    }

    #[test]
    fn a_busy_lock_is_waited_for_until_a_tunnel_starts() {
        let temp = TempDir::new().unwrap();
        let _lock = hold_lock(&temp);
        let (waiting_tx, waiting_rx) = mpsc::channel();
        let config = dir(&temp);
        let waiter = std::thread::spawn(move || {
            wait_for(&config, &name(), || waiting_tx.send(()).unwrap()).unwrap()
        });
        waiting_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("it said it was waiting");

        // The lock holder turns out to be a tunnel, which now says where it is.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _published = RunFile::new(listener.local_addr().unwrap(), TOKEN, "wireguard")
            .publish(&dir(&temp), &name())
            .unwrap();
        let access = waiter.join().unwrap();
        assert!(matches!(access, Access::Running(_)), "{access:?}");
    }

    #[test]
    fn a_busy_lock_is_waited_for_until_it_is_free() {
        let temp = TempDir::new().unwrap();
        let lock = hold_lock(&temp);
        let (waiting_tx, waiting_rx) = mpsc::channel();
        let config = dir(&temp);
        let waiter = std::thread::spawn(move || {
            wait_for(&config, &name(), || waiting_tx.send(()).unwrap()).unwrap()
        });
        waiting_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("it said it was waiting");
        drop(lock);
        let access = waiter.join().unwrap();
        assert!(matches!(access, Access::Free(_)), "{access:?}");
    }
}
