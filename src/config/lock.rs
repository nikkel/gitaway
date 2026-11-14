//! One tunnel per profile at a time.
//!
//! A WireGuard server keeps one session per device key. Two gitaway
//! commands using the same profile at once would share that key, and each
//! handshake would knock the other's session off. So a command takes the
//! profile's lock before opening its tunnel, and a second command waits.
//!
//! The lock is an operating-system file lock on `locks/<profile>.lock` in the
//! configuration directory. The OS releases it when the process exits,
//! however that happens (even `kill -9` or a crash), so a lock can never be
//! left stale. Different profiles never wait for each other.
//!
//! A running `gitaway tunnel` holds its profile's lock for as long as it
//! runs, and lends its tunnel to other commands instead (see
//! [`super::run_file`]). Those commands use [`ProfileLock::try_acquire`] to
//! tell whether to open their own tunnel or look for one to borrow.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use super::name::ProfileName;
use super::paths::ConfigDir;
use super::private_fs::create_private_dir;

/// A held profile lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct ProfileLock {
    _file: File,
}

/// The lock couldn't be taken.
#[derive(Debug, thiserror::Error)]
#[error("couldn't lock {}: {source}", path.display())]
pub struct LockError {
    path: PathBuf,
    source: io::Error,
}

impl ProfileLock {
    /// Takes the profile's lock, waiting while another process holds it.
    /// `on_wait` runs once, before waiting, so the user knows why nothing is
    /// happening.
    pub fn acquire(
        dir: &ConfigDir,
        name: &ProfileName,
        on_wait: impl FnOnce(),
    ) -> Result<Self, LockError> {
        let path = dir.lock_file(name);
        let file = open(dir, &path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                on_wait();
                file.lock().map_err(|source| LockError { path, source })?;
            }
            Err(std::fs::TryLockError::Error(source)) => return Err(LockError { path, source }),
        }
        Ok(Self { _file: file })
    }

    /// Takes the profile's lock if it's free, without waiting: `None` if
    /// another process holds it.
    pub fn try_acquire(dir: &ConfigDir, name: &ProfileName) -> Result<Option<Self>, LockError> {
        let path = dir.lock_file(name);
        let file = open(dir, &path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(source)) => Err(LockError { path, source }),
        }
    }
}

/// Opens the lock file at `path`, creating it (mode 600) and its directory
/// if needed.
fn open(dir: &ConfigDir, path: &Path) -> Result<File, LockError> {
    let err = |source| LockError {
        path: path.to_owned(),
        source,
    };
    if let Some(parent) = path.parent() {
        create_private_dir(dir.path(), parent).map_err(err)?;
    }
    let mut options = File::options();
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path).map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use tempfile::TempDir;

    fn dir(temp: &TempDir) -> ConfigDir {
        ConfigDir::new(Some(temp.path().into()), None).unwrap()
    }

    fn name(s: &str) -> ProfileName {
        s.parse().unwrap()
    }

    #[test]
    fn a_second_holder_waits_until_the_first_lets_go() {
        let temp = TempDir::new().unwrap();
        let first =
            ProfileLock::acquire(&dir(&temp), &name("home"), || panic!("nothing to wait for"))
                .unwrap();

        let (waited_tx, waited_rx) = mpsc::channel();
        let (got_tx, got_rx) = mpsc::channel();
        let config = dir(&temp);
        let second = std::thread::spawn(move || {
            let lock = ProfileLock::acquire(&config, &name("home"), || waited_tx.send(()).unwrap())
                .unwrap();
            got_tx.send(()).unwrap();
            drop(lock);
        });

        waited_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the second holder said it was waiting");
        assert!(
            got_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "it must not get the lock yet"
        );
        drop(first);
        got_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("it gets the lock once released");
        second.join().unwrap();
    }

    #[test]
    fn trying_a_held_lock_gives_none_until_it_is_released() {
        let temp = TempDir::new().unwrap();
        let first = ProfileLock::try_acquire(&dir(&temp), &name("home"))
            .unwrap()
            .expect("the lock is free");
        // Another process in real use; a thread here, since the lock belongs
        // to the open file, not the process.
        let config = dir(&temp);
        let tried = std::thread::spawn(move || {
            ProfileLock::try_acquire(&config, &name("home"))
                .unwrap()
                .is_some()
        });
        assert!(!tried.join().unwrap(), "a held lock must not be taken");
        drop(first);
        assert!(
            ProfileLock::try_acquire(&dir(&temp), &name("home"))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn different_profiles_never_wait() {
        let temp = TempDir::new().unwrap();
        let _home = ProfileLock::acquire(&dir(&temp), &name("home"), || panic!("no wait")).unwrap();
        let _office =
            ProfileLock::acquire(&dir(&temp), &name("office"), || panic!("no wait")).unwrap();
    }
}
