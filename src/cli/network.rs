//! What happens to a network command (clone, fetch, pull, push …).
//!
//! [`plan`] decides how to run git:
//!
//! ```text
//! no profiles configured ─────────────────────────► Direct: git as typed
//! find the remote URL ─► pick the profile (config::resolve)
//!   no profile, unknown remote, or the profile's
//!   [hosts] doesn't handle the remote's host ──────► Direct: git as typed
//!   home check passes ─────────────────────────────► Direct (clone also pins the profile)
//!   home check fails, or the profile has no [home] ► Away: through the tunnel (super::away)
//! ```
//!
//! A different host key at the home address also means away: another
//! network can have its own SSH server there. The tunnel is authenticated by
//! WireGuard, so it's safe either way, and a note says the key may have
//! changed in case you are home.

use std::collections::BTreeMap;
use std::ffi::OsString;

use crate::config::name::ProfileName;
use crate::config::profile::Profile;
use crate::config::resolve::{self, Inputs};
use crate::error::Error;
use crate::git::classify::{Invocation, RemoteArg};
use crate::git::remote_url::{self, RemoteUrl};
use crate::git::repo::{PROFILE_KEY, Repo};
use crate::git::runner::{Git, RunError};
use crate::home_check::{self, AwayReason, Verdict};

/// The environment variable that names a profile.
pub const PROFILE_ENV: &str = "GITAWAY_PROFILE";

/// How to run a network command.
#[derive(Debug)]
pub enum Plan {
    /// Run git with these arguments, directly.
    Direct(Vec<OsString>),
    /// Run git with these arguments through the profile's tunnel.
    Away(Box<AwayPlan>),
}

/// Everything the away path needs.
#[derive(Debug)]
pub struct AwayPlan {
    /// The git arguments, including global options.
    pub args: Vec<OsString>,
    /// git's global options alone, for gitaway's own git queries.
    pub global_options: Vec<OsString>,
    /// The profile's name.
    pub name: ProfileName,
    /// The profile.
    pub profile: Profile,
    /// The remote the command talks to.
    pub remote: RemoteUrl,
}

/// Decides how to run a network command.
///
/// `invocation` is the classified command line, and `profile_flag` the value
/// of `--profile`, if given.
pub fn plan(
    invocation: &Invocation<'_>,
    profile_flag: Option<&ProfileName>,
) -> Result<Plan, Error> {
    let as_typed = as_typed(invocation);
    let direct = |args| Ok(Plan::Direct(args));
    let env_profile = env_profile()?;

    let store = super::open_store()?;
    let profiles = store.load_profiles()?;
    if profiles.is_empty() && profile_flag.is_none() && env_profile.is_none() {
        return direct(as_typed);
    }
    let global = store.load_global()?;

    let repo = Repo::new(Git::new(invocation.global_options));
    let (git_config_profile, url) = match query_repo(&repo, invocation) {
        Ok(facts) => facts,
        // A lookup git itself refused (a bad `-C dir`, a broken config file)
        // fails the real command in exactly the same way, before any network
        // access. Let git report it, with its own message and exit code.
        Err(RunError::Failed { .. }) => return direct(as_typed),
        Err(e) => return Err(e.into()),
    };
    let git_config_profile = git_config_profile
        .map(|name| name.parse::<ProfileName>())
        .transpose()
        .map_err(|e| Error::msg(format!("git config {PROFILE_KEY}: {e}")))?;
    let remote = url.as_deref().and_then(remote_url::parse);

    let inputs = Inputs {
        flag: profile_flag,
        env: env_profile.as_ref(),
        git_config: git_config_profile.as_ref(),
        remote: remote.as_ref(),
        default: global.default_profile.as_ref(),
    };
    // Home checks run while resolving are kept, so none runs twice.
    let mut verdicts: BTreeMap<ProfileName, Verdict> = BTreeMap::new();
    let resolved = resolve::resolve(inputs, &profiles, |name, profile| {
        let verdict = verdict_if_checkable(profile);
        let home = verdict == Some(Verdict::Home);
        if let Some(verdict) = verdict {
            verdicts.insert(name.clone(), verdict);
        }
        home
    })?;

    let Some(resolved) = resolved else {
        return direct(as_typed);
    };
    let Some(remote) = remote else {
        return direct(as_typed);
    };
    if !resolved.profile.hosts.handles(&remote) {
        return direct(as_typed);
    }

    let verdict = match verdicts.remove(resolved.name) {
        Some(verdict) => Some(verdict),
        None => home_verdict(resolved.profile)?,
    };
    let args = pin_clone(invocation, resolved.name).unwrap_or(as_typed);
    match verdict {
        Some(Verdict::Home) => direct(args),
        Some(Verdict::Away(reason)) => {
            if let AwayReason::WrongKey { fingerprint } = &reason {
                wrong_key_note(resolved.name, resolved.profile, fingerprint);
            }
            Ok(Plan::Away(Box::new(AwayPlan {
                args,
                global_options: invocation.global_options.to_vec(),
                name: resolved.name.clone(),
                profile: resolved.profile.clone(),
                remote,
            })))
        }
        // No [home]: gitaway can't tell, so it always uses the tunnel.
        None => Ok(Plan::Away(Box::new(AwayPlan {
            args,
            global_options: invocation.global_options.to_vec(),
            name: resolved.name.clone(),
            profile: resolved.profile.clone(),
            remote,
        }))),
    }
}

/// The command exactly as typed (after gitaway's own `--profile`).
fn as_typed(invocation: &Invocation<'_>) -> Vec<OsString> {
    let mut args = invocation.global_options.to_vec();
    args.extend(invocation.subcommand.map(OsString::from));
    args.extend(invocation.args.iter().cloned());
    args
}

/// `GITAWAY_PROFILE`, if set and not empty.
pub(crate) fn env_profile() -> Result<Option<ProfileName>, Error> {
    match std::env::var(PROFILE_ENV) {
        Ok(value) if !value.is_empty() => value
            .parse()
            .map(Some)
            .map_err(|e| Error::msg(format!("{PROFILE_ENV}: {e}"))),
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(Error::msg(format!("{PROFILE_ENV} isn't valid UTF-8")))
        }
    }
}

/// What the repository says: `gitaway.profile`, and the URL the command
/// will talk to.
fn query_repo(
    repo: &Repo,
    invocation: &Invocation<'_>,
) -> Result<(Option<String>, Option<String>), RunError> {
    Ok((repo.profile()?, remote_url_of(repo, invocation)?))
}

/// The URL the command will talk to: a remote or URL named in the
/// arguments, else git's default remote for this command. `None` for a
/// clone of a local path.
fn remote_url_of(repo: &Repo, invocation: &Invocation<'_>) -> Result<Option<String>, RunError> {
    let names = repo.remote_names()?;
    let direction = invocation.direction();
    let url = match invocation.remote_argument(|arg| names.iter().any(|name| name == arg)) {
        Some(RemoteArg::Url(url)) => Some(repo.expand_url(url)?),
        Some(RemoteArg::Name(name)) => repo.remote_url(name, direction)?,
        None if invocation.subcommand == Some("clone") => None,
        None => {
            let remote = repo.default_remote(direction)?;
            repo.remote_url(&remote, direction)?
        }
    };
    Ok(url)
}

/// Runs the home check for a profile that will be used. `None` if the
/// profile has no `[home]` to check.
fn home_verdict(profile: &Profile) -> Result<Option<Verdict>, Error> {
    match &profile.home {
        Some(home) => Ok(Some(home_check::check(home)?)),
        None => Ok(None),
    }
}

/// The home check for choosing between several matching profiles. Profiles
/// that can't be checked (no `[home]`, or the check couldn't run) count as
/// not home.
fn verdict_if_checkable(profile: &Profile) -> Option<Verdict> {
    profile
        .home
        .as_ref()
        .and_then(|home| home_check::check(home).ok())
}

/// For `clone`, adds `--config gitaway.profile=<name>` so the new repository
/// remembers its profile. `None` for other commands.
fn pin_clone(invocation: &Invocation<'_>, name: &ProfileName) -> Option<Vec<OsString>> {
    if invocation.subcommand != Some("clone") {
        return None;
    }
    let mut args = invocation.global_options.to_vec();
    args.push("clone".into());
    args.push("--config".into());
    args.push(format!("{PROFILE_KEY}={name}").into());
    args.extend(invocation.args.iter().cloned());
    Some(args)
}

/// The home address answered with a different key. Usually that's another
/// network's server at the same address, but it can mean the home server's
/// key changed, so say so.
fn wrong_key_note(name: &ProfileName, profile: &Profile, fingerprint: &str) {
    let server = profile
        .home
        .as_ref()
        .map_or_else(|| "the home server".to_owned(), |home| home.ssh.to_string());
    eprintln!(
        "gitaway: note: {server} answered with a different host key ({fingerprint}), so this \
         isn't profile `{name}`'s home network. Using the tunnel.\n\
         If you are home, the server's key may have changed: check it on the server, then update \
         it with `gitaway profile set {name} home.host_key '<new key>'`"
    );
}
