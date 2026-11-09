//! Profile names.
//!
//! A profile name becomes a file name (`profiles/<name>.toml`) and, later, a
//! keychain entry. So the allowed characters are deliberately narrow:
//! lower-case ASCII letters, digits, `-` and `_`, 1 to 64 characters, not
//! starting with `-`. That rules out path traversal (`../x`), hidden files and
//! names that look like command-line options.

use std::fmt;
use std::str::FromStr;

/// The longest allowed profile name.
pub const MAX_LEN: usize = 64;

/// A validated profile name.
///
/// ```
/// use gitaway::config::name::ProfileName;
///
/// let name: ProfileName = "home".parse().unwrap();
/// assert_eq!(name.as_str(), "home");
///
/// assert!("../etc".parse::<ProfileName>().is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Deserialize)]
#[serde(try_from = "String")]
pub struct ProfileName(String);

/// Why a profile name was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid profile name `{0}`: use 1-{MAX_LEN} lower-case letters, digits, `-` or `_`, not starting with `-`"
)]
pub struct InvalidName(String);

impl ProfileName {
    /// The name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ProfileName {
    type Err = InvalidName;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let valid = (1..=MAX_LEN).contains(&s.len())
            && !s.starts_with('-')
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'));
        if valid {
            Ok(Self(s.to_owned()))
        } else {
            Err(InvalidName(s.to_owned()))
        }
    }
}

impl TryFrom<String> for ProfileName {
    type Error = InvalidName;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl fmt::Display for ProfileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        let longest = "a".repeat(MAX_LEN);
        for name in ["home", "office-2", "my_place", "0", longest.as_str()] {
            assert!(name.parse::<ProfileName>().is_ok(), "{name}");
        }
    }

    #[test]
    fn invalid_names() {
        let too_long = "a".repeat(MAX_LEN + 1);
        for name in [
            "",
            too_long.as_str(),
            "Home",
            "-home",
            "../home",
            "home/office",
            ".hidden",
            "home.toml",
            "a b",
            "café",
            "home\0",
        ] {
            assert!(name.parse::<ProfileName>().is_err(), "{name:?}");
        }
    }
}
