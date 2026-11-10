//! Where device keys are kept: the OS keychain, or a private file.
//!
//! ```text
//! GITAWAY_KEY_STORE   where keys go
//! ─────────────────   ─────────────────────────────────────────────────────
//! (unset)             the OS keychain if there is one, else a file (with a warning)
//! keychain            the OS keychain; an error if there isn't one
//! file                ~/.config/gitaway/keys/<profile>.key, mode 600
//! ```
//!
//! The keychain is the macOS Keychain, the Windows Credential Manager, or the
//! Secret Service (GNOME Keyring, KWallet) on Linux. Each key is stored
//! under the service name `gitaway` and the profile's name. Headless
//! machines often have no Secret Service, which is what the file fallback is
//! for.
//!
//! Key files are refused if other users can read them, as `ssh` does.
//!
//! What's stored for a profile is its [`DeviceSecrets`], as text: the
//! device key in base64 on the first line, then `preshared=<base64>` if the
//! server uses a preshared key. Both live in one entry, so the macOS
//! Keychain asks for permission once, not twice, and entries written before
//! preshared keys existed (just the key) read as before.
//!
//! # Extending
//!
//! A new place to keep keys is a new [`Backend`] variant, with a branch in
//! each of [`KeyStore::load`], [`KeyStore::save`] and [`KeyStore::delete`].

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use super::device_key::DeviceKey;
use super::preshared_key::PresharedKey;
use crate::config::name::ProfileName;
use crate::config::paths::ConfigDir;
use crate::config::private_fs;

/// The environment variable that chooses where keys are kept.
pub const KEY_STORE_ENV: &str = "GITAWAY_KEY_STORE";

/// The keychain service name keys are stored under.
const KEYCHAIN_SERVICE: &str = "gitaway";

/// Where keys are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Backend {
    /// The operating system's keychain.
    Keychain,
    /// One mode-600 file per profile in the configuration directory.
    File,
}

/// A device key couldn't be read, written or deleted.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum KeyError {
    /// The keychain refused or failed.
    #[error(
        "{} couldn't {action} the device key: {reason}.\n\
         To keep device keys in files instead, set {KEY_STORE_ENV}=file",
        keychain_name()
    )]
    Keychain {
        /// What was being done: `read`, `save` or `delete`.
        action: &'static str,
        /// Why it failed.
        reason: String,
    },
    /// A key file couldn't be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        /// The key file.
        path: PathBuf,
        /// What went wrong.
        source: io::Error,
    },
    /// A key file that other users can read.
    #[error(
        "{} can be read by other users, so it isn't used. Restrict it with `chmod 600 {}`",
        path.display(),
        path.display()
    )]
    Unprotected {
        /// The key file.
        path: PathBuf,
    },
    /// The stored key isn't a valid key.
    #[error("the device key for profile `{name}` in {location} is damaged")]
    Damaged {
        /// The profile.
        name: ProfileName,
        /// Where it's stored.
        location: String,
    },
    /// `GITAWAY_KEY_STORE` has a value gitaway doesn't know.
    #[error("{KEY_STORE_ENV} must be `keychain` or `file`, not `{0}`")]
    BadSetting(String),
    /// `GITAWAY_KEY_STORE=keychain`, but there's no keychain.
    #[error("{KEY_STORE_ENV}=keychain, but no keychain is available here")]
    NoKeychain,
}

/// The secrets a profile's tunnel needs, kept together in the key store.
#[derive(Debug)]
pub struct DeviceSecrets {
    /// This device's private key.
    pub key: DeviceKey,
    /// The preshared key, if the server uses one for this device.
    pub preshared_key: Option<PresharedKey>,
}

impl From<DeviceKey> for DeviceSecrets {
    fn from(key: DeviceKey) -> Self {
        Self::new(key)
    }
}

/// The label of the preshared key's line in the stored text.
const PRESHARED_PREFIX: &str = "preshared=";

impl DeviceSecrets {
    /// Just a device key, with no preshared key.
    pub fn new(key: DeviceKey) -> Self {
        Self {
            key,
            preshared_key: None,
        }
    }

    /// A second copy, for when one goes to the tunnel and one to the key
    /// store. Both are wiped when dropped.
    pub(crate) fn duplicate(&self) -> Self {
        Self {
            key: self.key.duplicate(),
            preshared_key: self.preshared_key.as_ref().map(PresharedKey::duplicate),
        }
    }

    /// The stored form. The text is wiped when dropped.
    fn to_text(&self) -> Zeroizing<String> {
        let mut text = Zeroizing::new(self.key.to_base64().as_str().to_owned());
        if let Some(preshared) = &self.preshared_key {
            text.push('\n');
            text.push_str(PRESHARED_PREFIX);
            text.push_str(&preshared.to_base64());
        }
        text
    }

    /// Reads the stored form, or `None` if it's damaged.
    fn from_text(text: &str) -> Option<Self> {
        let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
        let key = DeviceKey::from_base64(lines.next()?).ok()?;
        let preshared_key = match lines.next() {
            None => None,
            Some(line) => {
                Some(PresharedKey::from_base64(line.strip_prefix(PRESHARED_PREFIX)?).ok()?)
            }
        };
        if lines.next().is_some() {
            return None;
        }
        Some(Self { key, preshared_key })
    }
}

/// Reads and writes device keys, wherever they're kept.
#[derive(Debug, Clone)]
pub struct KeyStore {
    backend: Backend,
    dir: ConfigDir,
    fell_back: bool,
}

impl KeyStore {
    /// The store chosen by `GITAWAY_KEY_STORE`.
    pub fn from_env(dir: ConfigDir) -> Result<Self, KeyError> {
        Self::open(dir, std::env::var_os(KEY_STORE_ENV))
    }

    /// The store chosen by `setting`, the value of `GITAWAY_KEY_STORE`.
    pub fn open(dir: ConfigDir, setting: Option<OsString>) -> Result<Self, KeyError> {
        let setting = setting.map(|s| s.to_string_lossy().into_owned());
        let keychain_available = || keyring::Entry::store_status().is_ok();
        let (backend, fell_back) = match setting.as_deref() {
            None | Some("") if keychain_available() => (Backend::Keychain, false),
            None | Some("") => (Backend::File, true),
            Some("keychain") if keychain_available() => (Backend::Keychain, false),
            Some("keychain") => return Err(KeyError::NoKeychain),
            Some("file") => (Backend::File, false),
            Some(other) => return Err(KeyError::BadSetting(other.to_owned())),
        };
        Ok(Self {
            backend,
            dir,
            fell_back,
        })
    }

    /// Where keys go.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// Whether keys go to a file only because no keychain was found. Commands
    /// that save a key warn about it.
    pub fn fell_back_to_file(&self) -> bool {
        self.fell_back
    }

    /// Where a profile's key is kept, for messages.
    pub fn location(&self, name: &ProfileName) -> String {
        match self.backend {
            Backend::Keychain => keychain_name().to_owned(),
            Backend::File => self.dir.key_file(name).display().to_string(),
        }
    }

    /// The profile's secrets, or `None` if it has none.
    ///
    /// With the keychain, a key file is also checked, in case the key was
    /// saved before a keychain was available.
    pub fn load(&self, name: &ProfileName) -> Result<Option<DeviceSecrets>, KeyError> {
        if self.backend == Backend::Keychain
            && let Some(text) = keychain_get(name)?
        {
            return parse(&text, name, keychain_name()).map(Some);
        }
        let path = self.dir.key_file(name);
        match read_key_file(&path)? {
            Some(text) => parse(&text, name, &path.display().to_string()).map(Some),
            None => Ok(None),
        }
    }

    /// Saves the profile's secrets, replacing any it had. With the keychain,
    /// an old key file for the profile is deleted, so only one copy exists.
    pub fn save(&self, name: &ProfileName, secrets: &DeviceSecrets) -> Result<(), KeyError> {
        let text = secrets.to_text();
        match self.backend {
            Backend::Keychain => {
                keychain_entry(name, "save")?
                    .set_password(&text)
                    .map_err(|e| keychain_error("save", e))?;
                remove_key_file(&self.dir.key_file(name))?;
            }
            Backend::File => {
                let path = self.dir.key_file(name);
                let contents = Zeroizing::new(format!("{}\n", text.as_str()));
                private_fs::write_atomic(&path, contents.as_bytes())
                    .map_err(|source| KeyError::Io { path, source })?;
            }
        }
        Ok(())
    }

    /// Deletes the profile's key wherever it's kept. Returns whether there
    /// was one.
    pub fn delete(&self, name: &ProfileName) -> Result<bool, KeyError> {
        let mut deleted = false;
        if self.backend == Backend::Keychain {
            match keychain_entry(name, "delete")?.delete_credential() {
                Ok(()) => deleted = true,
                Err(keyring::Error::NoEntry) => {}
                Err(e) => return Err(keychain_error("delete", e)),
            }
        }
        deleted |= remove_key_file(&self.dir.key_file(name))?;
        Ok(deleted)
    }
}

fn parse(text: &str, name: &ProfileName, location: &str) -> Result<DeviceSecrets, KeyError> {
    DeviceSecrets::from_text(text).ok_or_else(|| KeyError::Damaged {
        name: name.clone(),
        location: location.to_owned(),
    })
}

fn keychain_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "the macOS Keychain"
    } else if cfg!(windows) {
        "the Windows Credential Manager"
    } else {
        "the Secret Service keyring"
    }
}

fn keychain_entry(name: &ProfileName, action: &'static str) -> Result<keyring::Entry, KeyError> {
    keyring::Entry::new(KEYCHAIN_SERVICE, name.as_str()).map_err(|e| keychain_error(action, e))
}

fn keychain_get(name: &ProfileName) -> Result<Option<Zeroizing<String>>, KeyError> {
    match keychain_entry(name, "read")?.get_password() {
        Ok(text) => Ok(Some(Zeroizing::new(text))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(keychain_error("read", e)),
    }
}

/// Describes a keychain error. Errors carrying the stored bytes are
/// described without them, so a damaged key never reaches a message.
fn keychain_error(action: &'static str, error: keyring::Error) -> KeyError {
    let reason = match error {
        keyring::Error::NoStorageAccess(e) => format!("access was refused ({e})"),
        keyring::Error::PlatformFailure(e) => e.to_string(),
        keyring::Error::BadEncoding(_) | keyring::Error::BadDataFormat(..) => {
            "the stored value isn't readable".to_owned()
        }
        keyring::Error::NoDefaultStore => "no keychain is available".to_owned(),
        other => other.to_string(),
    };
    KeyError::Keychain { action, reason }
}

/// Reads a key file, refusing it if other users could read it.
fn read_key_file(path: &Path) -> Result<Option<Zeroizing<String>>, KeyError> {
    let io_err = |source| KeyError::Io {
        path: path.to_owned(),
        source,
    };
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err(e)),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(KeyError::Unprotected {
                path: path.to_owned(),
            });
        }
    }
    #[cfg(not(unix))]
    let _ = metadata;
    std::fs::read_to_string(path)
        .map(|text| Some(Zeroizing::new(text)))
        .map_err(io_err)
}

fn remove_key_file(path: &Path) -> Result<bool, KeyError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(KeyError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

/// The keychain backend can't be tested automatically: it would touch the
/// real keychain of whoever runs the tests. These cover the file backend and
/// the choice of backend.
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn file_store() -> (TempDir, KeyStore) {
        let temp = TempDir::new().unwrap();
        let dir = ConfigDir::new(Some(temp.path().into()), None).unwrap();
        let store = KeyStore::open(dir, Some("file".into())).unwrap();
        (temp, store)
    }

    fn name(s: &str) -> ProfileName {
        s.parse().unwrap()
    }

    #[test]
    fn file_keys_can_be_saved_loaded_replaced_and_deleted() {
        let (_temp, store) = file_store();
        let home = name("home");
        assert!(store.load(&home).unwrap().is_none());

        let first = DeviceKey::generate();
        let first_public = first.public_key();
        store.save(&home, &DeviceSecrets::new(first)).unwrap();
        let loaded = store.load(&home).unwrap().unwrap();
        assert_eq!(loaded.key.public_key(), first_public);
        assert!(loaded.preshared_key.is_none());

        let second = DeviceSecrets {
            key: DeviceKey::generate(),
            preshared_key: Some(PresharedKey::from_base64(PRESHARED).unwrap()),
        };
        store.save(&home, &second).unwrap();
        let loaded = store.load(&home).unwrap().unwrap();
        assert_eq!(loaded.key.public_key(), second.key.public_key());
        assert_eq!(
            loaded.preshared_key.unwrap().to_base64().as_str(),
            PRESHARED
        );

        assert!(store.delete(&home).unwrap());
        assert!(store.load(&home).unwrap().is_none());
        assert!(!store.delete(&home).unwrap());
    }

    #[test]
    fn keys_are_per_profile() {
        let (_temp, store) = file_store();
        let secrets = DeviceSecrets::new(DeviceKey::generate());
        store.save(&name("home"), &secrets).unwrap();
        assert!(store.load(&name("office")).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn key_files_are_private_and_readable_ones_are_refused() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, store) = file_store();
        let home = name("home");
        store
            .save(&home, &DeviceSecrets::new(DeviceKey::generate()))
            .unwrap();
        let path = store.dir.key_file(&home);
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            store.load(&home),
            Err(KeyError::Unprotected { .. })
        ));
    }

    const PRESHARED: &str = "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA=";

    #[test]
    fn a_key_file_written_before_preshared_keys_still_reads() {
        let (_temp, store) = file_store();
        let home = name("home");
        let key = DeviceKey::generate();
        let text = format!("{}\n", key.to_base64().as_str());
        private_fs::write_atomic(&store.dir.key_file(&home), text.as_bytes()).unwrap();
        let loaded = store.load(&home).unwrap().unwrap();
        assert_eq!(loaded.key.public_key(), key.public_key());
        assert!(loaded.preshared_key.is_none());
    }

    #[test]
    fn stored_text_with_a_bad_second_or_extra_line_is_damaged() {
        let key = DeviceKey::generate().to_base64();
        let key = key.as_str();
        for text in [
            format!("{key}\n{PRESHARED}"),
            format!("{key}\npreshared=AAAA"),
            format!("{key}\npreshared={PRESHARED}\nmore"),
        ] {
            assert!(DeviceSecrets::from_text(&text).is_none(), "{text}");
        }
        let good = format!("{key}\npreshared={PRESHARED}\n");
        assert!(
            DeviceSecrets::from_text(&good)
                .unwrap()
                .preshared_key
                .is_some()
        );
    }

    #[test]
    fn a_damaged_key_file_is_an_error_without_its_contents() {
        let (_temp, store) = file_store();
        let home = name("home");
        let path = store.dir.key_file(&home);
        private_fs::write_atomic(&path, b"secret-looking-junk").unwrap();
        let message = store.load(&home).unwrap_err().to_string();
        assert!(message.contains("damaged"), "{message}");
        assert!(!message.contains("secret-looking-junk"), "{message}");
    }

    #[test]
    fn the_setting_is_validated() {
        let temp = TempDir::new().unwrap();
        let dir = ConfigDir::new(Some(temp.path().into()), None).unwrap();
        assert!(matches!(
            KeyStore::open(dir.clone(), Some("vault".into())),
            Err(KeyError::BadSetting(value)) if value == "vault"
        ));
        let store = KeyStore::open(dir, Some("file".into())).unwrap();
        assert_eq!(store.backend(), Backend::File);
        assert!(!store.fell_back_to_file());
    }
}
