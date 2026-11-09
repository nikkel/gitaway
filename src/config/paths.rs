//! Where gitaway's configuration lives.
//!
//! The directory is `$XDG_CONFIG_HOME/gitaway` if that variable is set to an
//! absolute path, otherwise `~/.config/gitaway`, on every platform. macOS
//! tools often use `~/Library/Application Support` instead, but CLI tools
//! conventionally use `~/.config`, and one location everywhere keeps the docs
//! simple.
//!
//! [`ConfigDir::new`] takes the environment as arguments, so tests never
//! depend on the real environment. [`ConfigDir::from_env`] is the thin wrapper
//! the program uses.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::name::ProfileName;

/// gitaway's configuration directory and the files inside it.
///
/// ```
/// use std::path::{Path, PathBuf};
/// use gitaway::config::paths::ConfigDir;
///
/// let dir = ConfigDir::new(None, Some(PathBuf::from("/home/alice"))).unwrap();
/// assert_eq!(dir.path(), Path::new("/home/alice/.config/gitaway"));
/// assert_eq!(dir.config_file(), Path::new("/home/alice/.config/gitaway/config.toml"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigDir {
    root: PathBuf,
}

/// Neither `XDG_CONFIG_HOME` nor a home directory could be found.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("can't find a configuration directory: set HOME or XDG_CONFIG_HOME")]
pub struct NoConfigDir;

impl ConfigDir {
    /// Works out the directory from the value of `XDG_CONFIG_HOME` and the
    /// user's home directory. A relative or empty `XDG_CONFIG_HOME` is
    /// ignored, as the XDG specification requires.
    pub fn new(
        xdg_config_home: Option<OsString>,
        home: Option<PathBuf>,
    ) -> Result<Self, NoConfigDir> {
        let base = xdg_config_home
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| home.map(|home| home.join(".config")))
            .ok_or(NoConfigDir)?;
        Ok(Self {
            root: base.join("gitaway"),
        })
    }

    /// Works out the directory from the process environment.
    pub fn from_env() -> Result<Self, NoConfigDir> {
        Self::new(std::env::var_os("XDG_CONFIG_HOME"), std::env::home_dir())
    }

    /// The directory itself.
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// `config.toml`, the settings that aren't per profile.
    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// The directory holding one TOML file per profile.
    pub fn profiles_dir(&self) -> PathBuf {
        self.root.join("profiles")
    }

    /// The file for one profile. The name is already validated, so it can't
    /// escape the profiles directory.
    pub fn profile_file(&self, name: &ProfileName) -> PathBuf {
        self.profiles_dir().join(format!("{name}.toml"))
    }

    /// The directory for device keys, when they're kept in files rather than
    /// the OS keychain.
    pub fn keys_dir(&self) -> PathBuf {
        self.root.join("keys")
    }

    /// The key file for one profile.
    pub fn key_file(&self, name: &ProfileName) -> PathBuf {
        self.keys_dir().join(format!("{name}.key"))
    }

    /// The lock file that stops two commands using one profile's tunnel at
    /// once.
    pub fn lock_file(&self, name: &ProfileName) -> PathBuf {
        self.root.join("locks").join(format!("{name}.lock"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> Option<PathBuf> {
        Some(PathBuf::from("/home/alice"))
    }

    #[test]
    fn absolute_xdg_config_home_wins() {
        let dir = ConfigDir::new(Some("/custom".into()), home()).unwrap();
        assert_eq!(dir.path(), Path::new("/custom/gitaway"));
    }

    #[test]
    fn relative_or_empty_xdg_config_home_is_ignored() {
        for xdg in ["relative/dir", ""] {
            let dir = ConfigDir::new(Some(xdg.into()), home()).unwrap();
            assert_eq!(
                dir.path(),
                Path::new("/home/alice/.config/gitaway"),
                "{xdg:?}"
            );
        }
    }

    #[test]
    fn no_home_and_no_xdg_is_an_error() {
        assert_eq!(ConfigDir::new(None, None), Err(NoConfigDir));
        assert_eq!(
            ConfigDir::new(Some("relative".into()), None),
            Err(NoConfigDir)
        );
    }

    #[test]
    fn profile_files_live_in_the_profiles_dir() {
        let dir = ConfigDir::new(None, home()).unwrap();
        let name = "office".parse().unwrap();
        assert_eq!(
            dir.profile_file(&name),
            Path::new("/home/alice/.config/gitaway/profiles/office.toml")
        );
        assert_eq!(
            dir.key_file(&name),
            Path::new("/home/alice/.config/gitaway/keys/office.key")
        );
    }
}
