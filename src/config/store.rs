//! Load, create, edit and remove profiles and `config.toml` on disk.
//!
//! This is the only module that reads or writes gitaway's configuration
//! files. Everything else works with the parsed types from [`super::profile`].
//!
//! # Safety guarantees
//!
//! - **No half-written files.** Every write is atomic (see
//!   `config::private_fs`): after a crash or power cut the file holds either
//!   the old contents or the new, never a mix.
//! - **No invalid files.** Edits are made on the TOML document (so the user's
//!   comments and layout survive) and the result is validated as a profile
//!   before anything is written. An invalid edit changes nothing.
//! - **Private by default.** Files are created with mode 600 and directories
//!   with mode 700 on Unix. Profiles hold no secrets, but they do describe
//!   your network.
//! - **Loud failures.** A profile that doesn't parse, or a `.toml` file whose
//!   name isn't a valid profile name, is an error naming the file, rather
//!   than being skipped.
//!
//! # Extending
//!
//! A new file kind gets its own `load_*` / `update_*` pair built on
//! the private `read_optional` and `write_atomic` helpers, so it inherits the
//! guarantees above.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use toml_edit::DocumentMut;

use super::name::ProfileName;
use super::paths::ConfigDir;
use super::private_fs;
use super::profile::{GlobalConfig, ParseError, Profile};

/// Something went wrong reading or writing configuration files.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The file system refused an operation.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or directory involved.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// A file exists but isn't valid.
    #[error("{}: {source}", path.display())]
    Parse {
        /// The file that failed to parse.
        path: PathBuf,
        /// What's wrong with it.
        source: ParseError,
    },
    /// A `.toml` file in the profiles directory has a name that isn't a valid
    /// profile name.
    #[error(
        "{}: not a valid profile name; rename it (lower-case letters, digits, `-`, `_`) or move it out of the profiles directory",
        .0.display()
    )]
    BadFileName(PathBuf),
    /// `create_profile` was asked to create a profile that already exists.
    #[error("profile `{0}` already exists")]
    AlreadyExists(ProfileName),
    /// The named profile doesn't exist.
    #[error("profile `{0}` doesn't exist; see `gitaway profile list`")]
    NotFound(ProfileName),
    /// An edit would leave the file invalid, so nothing was written.
    #[error("not saved, the result would be invalid: {0}")]
    InvalidEdit(ParseError),
}

/// Reads and writes the files in a [`ConfigDir`].
///
/// ```no_run
/// use gitaway::config::{paths::ConfigDir, store::Store};
///
/// let store = Store::new(ConfigDir::from_env()?);
/// for (name, _profile) in store.load_profiles()? {
///     println!("{name}");
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
pub struct Store {
    dir: ConfigDir,
}

impl Store {
    /// A store for the files in `dir`. Nothing is read until asked.
    pub fn new(dir: ConfigDir) -> Self {
        Self { dir }
    }

    /// The directory this store works in.
    pub fn dir(&self) -> &ConfigDir {
        &self.dir
    }

    /// Loads `config.toml`. A missing file means all defaults.
    pub fn load_global(&self) -> Result<GlobalConfig, StoreError> {
        let path = self.dir.config_file();
        match read_optional(&path)? {
            Some(text) => {
                GlobalConfig::from_toml(&text).map_err(|source| StoreError::Parse { path, source })
            }
            None => Ok(GlobalConfig::default()),
        }
    }

    /// Edits `config.toml`, keeping its comments and layout, and saves it if
    /// the result is valid. Creates the file if it doesn't exist.
    pub fn update_global(
        &self,
        edit: impl FnOnce(&mut DocumentMut) -> Result<(), String>,
    ) -> Result<(), StoreError> {
        let path = self.dir.config_file();
        let text = read_optional(&path)?.unwrap_or_default();
        let new_text = apply_edit(&path, &text, edit)?;
        GlobalConfig::from_toml(&new_text).map_err(StoreError::InvalidEdit)?;
        write_atomic(&path, &new_text)
    }

    /// Loads every profile. A missing profiles directory means no profiles.
    ///
    /// Files that don't end in `.toml`, and hidden files, are ignored. Any
    /// profile that fails to load is an error, because skipping it could
    /// change which profile a command picks.
    pub fn load_profiles(&self) -> Result<BTreeMap<ProfileName, Profile>, StoreError> {
        let dir = self.dir.profiles_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(source) => return Err(StoreError::Io { path: dir, source }),
        };

        let mut profiles = BTreeMap::new();
        for entry in entries {
            let path = entry
                .map_err(|source| StoreError::Io {
                    path: dir.clone(),
                    source,
                })?
                .path();
            let Some(stem) = profile_stem(&path) else {
                continue;
            };
            let name: ProfileName = stem
                .parse()
                .map_err(|_| StoreError::BadFileName(path.clone()))?;
            let profile = self.load_profile(&name)?;
            profiles.insert(name, profile);
        }
        Ok(profiles)
    }

    /// Loads one profile.
    pub fn load_profile(&self, name: &ProfileName) -> Result<Profile, StoreError> {
        let path = self.dir.profile_file(name);
        let text = read_optional(&path)?.ok_or_else(|| StoreError::NotFound(name.clone()))?;
        Profile::from_toml(&text).map_err(|source| StoreError::Parse { path, source })
    }

    /// Whether a profile file exists.
    pub fn profile_exists(&self, name: &ProfileName) -> bool {
        self.dir.profile_file(name).is_file()
    }

    /// Creates a new profile from TOML text, if it's valid and no profile with
    /// that name exists.
    pub fn create_profile(&self, name: &ProfileName, text: &str) -> Result<(), StoreError> {
        if self.profile_exists(name) {
            return Err(StoreError::AlreadyExists(name.clone()));
        }
        Profile::from_toml(text).map_err(StoreError::InvalidEdit)?;
        write_atomic(&self.dir.profile_file(name), text)
    }

    /// Edits a profile, keeping its comments and layout, and saves it only if
    /// `edit` succeeds and the result is a valid profile.
    pub fn update_profile(
        &self,
        name: &ProfileName,
        edit: impl FnOnce(&mut DocumentMut) -> Result<(), String>,
    ) -> Result<(), StoreError> {
        let path = self.dir.profile_file(name);
        let text = read_optional(&path)?.ok_or_else(|| StoreError::NotFound(name.clone()))?;
        let new_text = apply_edit(&path, &text, edit)?;
        Profile::from_toml(&new_text).map_err(StoreError::InvalidEdit)?;
        write_atomic(&path, &new_text)
    }

    /// Deletes a profile's file.
    ///
    /// This doesn't touch secrets or `default_profile`. The command that calls
    /// it is responsible for those.
    pub fn remove_profile(&self, name: &ProfileName) -> Result<(), StoreError> {
        let path = self.dir.profile_file(name);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Err(StoreError::NotFound(name.clone()))
            }
            Err(source) => Err(StoreError::Io { path, source }),
        }
    }
}

/// The profile name part of `<name>.toml`, or `None` for files to ignore.
fn profile_stem(path: &Path) -> Option<&str> {
    let file_name = path.file_name()?.to_str()?;
    if file_name.starts_with('.') {
        return None;
    }
    file_name.strip_suffix(".toml")
}

/// Parses `text` as an editable TOML document, applies `edit`, and returns
/// the new text. An `edit` that fails changes nothing.
fn apply_edit(
    path: &Path,
    text: &str,
    edit: impl FnOnce(&mut DocumentMut) -> Result<(), String>,
) -> Result<String, StoreError> {
    let mut doc: DocumentMut =
        text.parse()
            .map_err(|e: toml_edit::TomlError| StoreError::Parse {
                path: path.to_owned(),
                source: ParseError::Invalid(e.to_string()),
            })?;
    edit(&mut doc).map_err(|e| StoreError::InvalidEdit(ParseError::Invalid(e)))?;
    Ok(doc.to_string())
}

/// Reads a file, or returns `None` if it doesn't exist.
fn read_optional(path: &Path) -> Result<Option<String>, StoreError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(StoreError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

fn write_atomic(path: &Path, text: &str) -> Result<(), StoreError> {
    private_fs::write_atomic(path, text.as_bytes()).map_err(|source| StoreError::Io {
        path: path.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const PROFILE: &str = r#"# my comment
[transport.wireguard]
endpoint = "vpn.example.net:51820"
server_public_key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
"#;

    fn store() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let dir = ConfigDir::new(Some(tmp.path().into()), None).unwrap();
        (tmp, Store::new(dir))
    }

    fn name(s: &str) -> ProfileName {
        s.parse().unwrap()
    }

    #[test]
    fn empty_config_dir_means_defaults_and_no_profiles() {
        let (_tmp, store) = store();
        assert_eq!(store.load_global().unwrap(), GlobalConfig::default());
        assert!(store.load_profiles().unwrap().is_empty());
    }

    #[test]
    fn created_profiles_can_be_loaded() {
        let (_tmp, store) = store();
        store.create_profile(&name("home"), PROFILE).unwrap();
        store.create_profile(&name("office"), PROFILE).unwrap();

        let profiles = store.load_profiles().unwrap();
        let names: Vec<_> = profiles.keys().map(ProfileName::as_str).collect();
        assert_eq!(names, ["home", "office"]);
        assert!(store.profile_exists(&name("home")));
    }

    #[test]
    fn creating_an_existing_or_invalid_profile_fails() {
        let (_tmp, store) = store();
        store.create_profile(&name("home"), PROFILE).unwrap();
        assert!(matches!(
            store.create_profile(&name("home"), PROFILE),
            Err(StoreError::AlreadyExists(_))
        ));
        assert!(matches!(
            store.create_profile(&name("bad"), "not = [valid"),
            Err(StoreError::InvalidEdit(_))
        ));
        assert!(!store.profile_exists(&name("bad")));
    }

    #[test]
    fn valid_edits_keep_comments() {
        let (_tmp, store) = store();
        store.create_profile(&name("home"), PROFILE).unwrap();
        store
            .update_profile(&name("home"), |doc| {
                doc["transport"]["wireguard"]["mtu"] = toml_edit::value(1420);
                Ok(())
            })
            .unwrap();

        let text = fs::read_to_string(store.dir().profile_file(&name("home"))).unwrap();
        assert!(text.contains("# my comment"));
        let profile = store.load_profile(&name("home")).unwrap();
        let super::super::profile::Transport::WireGuard(wg) = profile.transport;
        assert_eq!(wg.mtu, Some(1420));
    }

    #[test]
    fn invalid_edits_change_nothing() {
        let (_tmp, store) = store();
        store.create_profile(&name("home"), PROFILE).unwrap();
        let path = store.dir().profile_file(&name("home"));
        let before = fs::read_to_string(&path).unwrap();

        let result = store.update_profile(&name("home"), |doc| {
            doc["transport"]["wireguard"]["endpoint"] = toml_edit::value("no port");
            Ok(())
        });

        assert!(matches!(result, Err(StoreError::InvalidEdit(_))));
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn failed_edits_change_nothing() {
        let (_tmp, store) = store();
        store.create_profile(&name("home"), PROFILE).unwrap();
        let path = store.dir().profile_file(&name("home"));
        let before = fs::read_to_string(&path).unwrap();

        let result = store.update_profile(&name("home"), |doc| {
            doc["transport"]["wireguard"]["mtu"] = toml_edit::value(1400);
            Err("the edit gave up".to_owned())
        });

        assert!(matches!(result, Err(StoreError::InvalidEdit(_))));
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn global_config_edits() {
        let (_tmp, store) = store();
        store
            .update_global(|doc| {
                doc["default_profile"] = toml_edit::value("home");
                Ok(())
            })
            .unwrap();
        assert_eq!(
            store.load_global().unwrap().default_profile,
            Some(name("home"))
        );

        let invalid = store.update_global(|doc| {
            doc["default_profile"] = toml_edit::value("../x");
            Ok(())
        });
        assert!(matches!(invalid, Err(StoreError::InvalidEdit(_))));
        assert_eq!(
            store.load_global().unwrap().default_profile,
            Some(name("home"))
        );
    }

    #[test]
    fn removing_profiles() {
        let (_tmp, store) = store();
        store.create_profile(&name("home"), PROFILE).unwrap();
        store.remove_profile(&name("home")).unwrap();
        assert!(!store.profile_exists(&name("home")));
        assert!(matches!(
            store.remove_profile(&name("home")),
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            store.load_profile(&name("home")),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn broken_profile_is_an_error_naming_the_file() {
        let (_tmp, store) = store();
        store.create_profile(&name("home"), PROFILE).unwrap();
        fs::write(store.dir().profile_file(&name("office")), "transport = 1").unwrap();

        let err = store.load_profiles().unwrap_err();
        assert!(matches!(&err, StoreError::Parse { path, .. } if path.ends_with("office.toml")));
    }

    #[test]
    fn badly_named_toml_file_is_an_error_but_other_files_are_ignored() {
        let (_tmp, store) = store();
        store.create_profile(&name("home"), PROFILE).unwrap();
        let profiles = store.dir().profiles_dir();
        fs::write(profiles.join("notes.txt"), "anything").unwrap();
        fs::write(profiles.join(".home.toml.swp"), "anything").unwrap();
        fs::write(profiles.join(".hidden.toml"), "anything").unwrap();
        assert_eq!(store.load_profiles().unwrap().len(), 1);

        // Not `Home.toml`: macOS file systems usually ignore case, so that
        // would just overwrite `home.toml`.
        fs::write(profiles.join("my home.toml"), PROFILE).unwrap();
        assert!(matches!(
            store.load_profiles(),
            Err(StoreError::BadFileName(_))
        ));
    }

    #[test]
    fn writes_leave_no_temporary_files() {
        let (_tmp, store) = store();
        store.create_profile(&name("home"), PROFILE).unwrap();
        let _ = store.update_profile(&name("home"), |doc| {
            doc["transport"]["wireguard"]["mtu"] = toml_edit::value(1400);
            Ok(())
        });
        let files: Vec<_> = fs::read_dir(store.dir().profiles_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(files, ["home.toml"]);
    }

    #[cfg(unix)]
    #[test]
    fn files_and_directories_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, store) = store();
        store.create_profile(&name("home"), PROFILE).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&store.dir().profile_file(&name("home"))), 0o600);
        assert_eq!(mode(&store.dir().profiles_dir()), 0o700);
    }
}
