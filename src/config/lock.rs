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

use std::fs::File;
use std::io;
use std::path::PathBuf;

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
        let err = |source| LockError {
            path: path.clone(),
            source,
        };
        if let Some(parent) = path.parent() {
            create_private_dir(parent).map_err(err)?;
        }
        let mut options = File::options();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options.open(&path).map_err(err)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                on_wait();
                file.lock().map_err(err)?;
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(err(e)),
        }
        Ok(Self { _file: file })
    }
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
    fn different_profiles_never_wait() {
        let temp = TempDir::new().unwrap();
        let _home = ProfileLock::acquire(&dir(&temp), &name("home"), || panic!("no wait")).unwrap();
        let _office =
            ProfileLock::acquire(&dir(&temp), &name("office"), || panic!("no wait")).unwrap();
    }
}
