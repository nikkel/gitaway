//! Decide which profile applies to a command.
//!
//! The first rule that applies wins:
//!
//! 1. `--profile <name>`
//! 2. the `GITAWAY_PROFILE` environment variable
//! 3. `git config gitaway.profile` (repo, then global, then `includeIf`)
//! 4. the one profile whose `[hosts]` handles the remote's URL
//! 5. `default_profile` in `config.toml`
//!
//! If several profiles handle the remote in step 4, each one's home check
//! runs. If exactly one passes, you're at that home and it's used. Otherwise
//! it's an error: away from home the candidates can't be told apart, and
//! gitaway never guesses.
//!
//! [`resolve`] is pure. The caller gathers the inputs (flags, environment,
//! git config, profiles) and passes the home check in as a function, so every
//! rule can be tested without a network, git or files.

use std::collections::BTreeMap;

use crate::git::remote_url::RemoteUrl;

use super::name::ProfileName;
use super::profile::Profile;

/// Everything that can name a profile, gathered by the caller.
#[derive(Debug, Clone, Copy, Default)]
pub struct Inputs<'a> {
    /// From `--profile`.
    pub flag: Option<&'a ProfileName>,
    /// From `GITAWAY_PROFILE`.
    pub env: Option<&'a ProfileName>,
    /// From `git config gitaway.profile`.
    pub git_config: Option<&'a ProfileName>,
    /// The remote the command talks to, if one could be worked out.
    pub remote: Option<&'a RemoteUrl>,
    /// From `default_profile` in `config.toml`.
    pub default: Option<&'a ProfileName>,
}

/// Which rule chose the profile. Used in messages, so a user can see why a
/// profile was picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Source {
    /// `--profile`.
    Flag,
    /// `GITAWAY_PROFILE`.
    Env,
    /// `git config gitaway.profile`.
    GitConfig,
    /// The remote's host matched one profile's `[hosts]`.
    HostMatch,
    /// The remote's host matched several profiles, and only this one's home
    /// check passed.
    VerifiedHome,
    /// `default_profile`.
    Default,
}

/// The chosen profile and why it was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved<'a> {
    /// The profile's name.
    pub name: &'a ProfileName,
    /// The profile.
    pub profile: &'a Profile,
    /// The rule that chose it.
    pub source: Source,
}

/// No profile could be chosen safely.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ResolveError {
    /// A profile was named explicitly but doesn't exist.
    #[error("profile `{name}` (from {origin}) doesn't exist; see `gitaway profile list`")]
    NotFound {
        /// The missing profile.
        name: ProfileName,
        /// Where the name came from, for the message.
        origin: &'static str,
    },
    /// Several profiles handle the remote, and their home checks couldn't
    /// tell them apart.
    #[error(
        "profiles {} all handle this remote and none is verifiably home; pick one with `gitaway use <profile>`",
        .0.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")
    )]
    Ambiguous(Vec<ProfileName>),
}

/// Picks the profile for a command. `Ok(None)` means no rule applies, and the
/// command runs as plain git.
///
/// `is_home` runs a profile's home check. It's only called when several
/// profiles handle the remote.
pub fn resolve<'a>(
    inputs: Inputs<'_>,
    profiles: &'a BTreeMap<ProfileName, Profile>,
    mut is_home: impl FnMut(&ProfileName, &Profile) -> bool,
) -> Result<Option<Resolved<'a>>, ResolveError> {
    let named = [
        (inputs.flag, Source::Flag, "--profile"),
        (inputs.env, Source::Env, "GITAWAY_PROFILE"),
        (
            inputs.git_config,
            Source::GitConfig,
            "git config gitaway.profile",
        ),
    ];
    for (name, source, label) in named {
        if let Some(name) = name {
            return lookup(profiles, name, source, label).map(Some);
        }
    }

    if let Some(remote) = inputs.remote {
        let candidates: Vec<_> = profiles
            .iter()
            .filter(|(_, profile)| profile.hosts.handles(remote))
            .collect();
        match candidates.as_slice() {
            [] => {}
            [(name, profile)] => {
                return Ok(Some(Resolved {
                    name,
                    profile,
                    source: Source::HostMatch,
                }));
            }
            several => {
                let mut home = several
                    .iter()
                    .filter(|(name, profile)| is_home(name, profile));
                return match (home.next(), home.next()) {
                    (Some((name, profile)), None) => Ok(Some(Resolved {
                        name,
                        profile,
                        source: Source::VerifiedHome,
                    })),
                    _ => Err(ResolveError::Ambiguous(
                        several.iter().map(|(name, _)| (*name).clone()).collect(),
                    )),
                };
            }
        }
    }

    match inputs.default {
        Some(name) => lookup(profiles, name, Source::Default, "default_profile").map(Some),
        None => Ok(None),
    }
}

fn lookup<'a>(
    profiles: &'a BTreeMap<ProfileName, Profile>,
    name: &ProfileName,
    source: Source,
    label: &'static str,
) -> Result<Resolved<'a>, ResolveError> {
    let (name, profile) = profiles
        .get_key_value(name)
        .ok_or_else(|| ResolveError::NotFound {
            name: name.clone(),
            origin: label,
        })?;
    Ok(Resolved {
        name,
        profile,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::remote_url;

    fn profile(hosts: &[&str]) -> Profile {
        let hosts: String = hosts
            .iter()
            .map(|host| format!("\"{host}\" = \"10.0.0.1\"\n"))
            .collect();
        Profile::from_toml(&format!(
            r#"
            [transport.wireguard]
            endpoint = "vpn.example.net:51820"
            server_public_key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
            [hosts]
            {hosts}
            "#
        ))
        .unwrap()
    }

    fn name(s: &str) -> ProfileName {
        s.parse().unwrap()
    }

    /// home and cabin both handle 192.0.2.10; office handles 198.51.100.5.
    fn profiles() -> BTreeMap<ProfileName, Profile> {
        BTreeMap::from([
            (name("home"), profile(&["192.0.2.10"])),
            (name("cabin"), profile(&["192.0.2.10"])),
            (name("office"), profile(&["198.51.100.5"])),
        ])
    }

    fn never_home(_: &ProfileName, _: &Profile) -> bool {
        panic!("home check should not run")
    }

    fn resolved_name(
        inputs: Inputs<'_>,
        is_home: impl FnMut(&ProfileName, &Profile) -> bool,
    ) -> Result<Option<String>, ResolveError> {
        let profiles = profiles();
        resolve(inputs, &profiles, is_home).map(|r| r.map(|r| r.name.to_string()))
    }

    #[test]
    fn explicit_sources_win_in_order() {
        let (flag, env, git, default) = (name("office"), name("home"), name("cabin"), name("home"));
        let all = Inputs {
            flag: Some(&flag),
            env: Some(&env),
            git_config: Some(&git),
            remote: None,
            default: Some(&default),
        };
        assert_eq!(resolved_name(all, never_home), Ok(Some("office".into())));

        let no_flag = Inputs { flag: None, ..all };
        assert_eq!(resolved_name(no_flag, never_home), Ok(Some("home".into())));

        let only_git = Inputs {
            env: None,
            ..no_flag
        };
        assert_eq!(
            resolved_name(only_git, never_home),
            Ok(Some("cabin".into()))
        );
    }

    #[test]
    fn explicit_source_beats_host_match() {
        let remote = remote_url::parse("ssh://git@198.51.100.5/app.git").unwrap();
        let git = name("home");
        let inputs = Inputs {
            git_config: Some(&git),
            remote: Some(&remote),
            ..Inputs::default()
        };
        assert_eq!(resolved_name(inputs, never_home), Ok(Some("home".into())));
    }

    #[test]
    fn unique_host_match_wins_over_default() {
        let remote = remote_url::parse("ssh://git@198.51.100.5/app.git").unwrap();
        let default = name("home");
        let inputs = Inputs {
            remote: Some(&remote),
            default: Some(&default),
            ..Inputs::default()
        };
        let profiles = profiles();
        let resolved = resolve(inputs, &profiles, never_home).unwrap().unwrap();
        assert_eq!(resolved.name.as_str(), "office");
        assert_eq!(resolved.source, Source::HostMatch);
    }

    #[test]
    fn several_matches_use_the_one_verified_home() {
        let remote = remote_url::parse("git@192.0.2.10:app.git").unwrap();
        let inputs = Inputs {
            remote: Some(&remote),
            ..Inputs::default()
        };
        let profiles = profiles();
        let resolved = resolve(inputs, &profiles, |name, _| name.as_str() == "cabin")
            .unwrap()
            .unwrap();
        assert_eq!(resolved.name.as_str(), "cabin");
        assert_eq!(resolved.source, Source::VerifiedHome);
    }

    #[test]
    fn several_matches_without_one_verified_home_is_an_error() {
        let remote = remote_url::parse("git@192.0.2.10:app.git").unwrap();
        let default = name("office");
        let inputs = Inputs {
            remote: Some(&remote),
            default: Some(&default),
            ..Inputs::default()
        };
        let expected = Err(ResolveError::Ambiguous(vec![name("cabin"), name("home")]));
        assert_eq!(resolved_name(inputs, |_, _| false), expected, "none home");
        assert_eq!(resolved_name(inputs, |_, _| true), expected, "both home");
    }

    #[test]
    fn unmatched_remote_falls_back_to_default() {
        let remote = remote_url::parse("git@github.com:alice/app.git").unwrap();
        let default = name("office");
        let inputs = Inputs {
            remote: Some(&remote),
            default: Some(&default),
            ..Inputs::default()
        };
        assert_eq!(resolved_name(inputs, never_home), Ok(Some("office".into())));
    }

    #[test]
    fn nothing_applies_means_plain_git() {
        let remote = remote_url::parse("git@github.com:alice/app.git").unwrap();
        let inputs = Inputs {
            remote: Some(&remote),
            ..Inputs::default()
        };
        assert_eq!(resolved_name(inputs, never_home), Ok(None));
        assert_eq!(resolved_name(Inputs::default(), never_home), Ok(None));
    }

    #[test]
    fn named_profile_that_does_not_exist_is_an_error() {
        let missing = name("missing");
        for inputs in [
            Inputs {
                flag: Some(&missing),
                ..Inputs::default()
            },
            Inputs {
                env: Some(&missing),
                ..Inputs::default()
            },
            Inputs {
                git_config: Some(&missing),
                ..Inputs::default()
            },
            Inputs {
                default: Some(&missing),
                ..Inputs::default()
            },
        ] {
            assert!(matches!(
                resolved_name(inputs, never_home),
                Err(ResolveError::NotFound { .. })
            ));
        }
    }
}
