//! What happens to a network command (clone, fetch, pull, push …).
//!
//! [`plan`] decides how to run git:
//!
//! ```text
//! no profiles configured ─────────────────────────► Direct: git as typed
//! find every URL the command may contact ─► pick the profile (config::resolve)
//!   another profile's home host among them ────────► error: one tunnel per command
//!   no profile, or the profile's [hosts]
//!   handles none of the URLs ──────────────────────► Direct: git as typed
//!   home check passes ─────────────────────────────► Direct (clone also pins the profile)
//!   home check fails, or the profile has no [home] ► Away: through the tunnel (super::away)
//! ```
//!
//! "Every URL" matters because one command can contact several servers: a
//! push to every push URL of a remote, `fetch --all` to every remote,
//! submodules to theirs. If any is a home host, the whole command goes
//! through the proxy, which still sends the others directly.
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

/// A URL a command may contact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    /// The URL as git will use it (after `insteadOf`).
    pub text: String,
    /// The same URL, parsed.
    pub url: RemoteUrl,
    /// The remote it belongs to; `None` for a URL typed in the arguments or
    /// a submodule's.
    pub remote: Option<String>,
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
    /// The command's URLs that the profile's `[hosts]` handles.
    pub home: Vec<Contact>,
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
    let (git_config_profile, urls) = match query_repo(&repo, invocation) {
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
    let mut contacts: Vec<Contact> = Vec::new();
    for (remote, text) in urls {
        let Some(url) = remote_url::parse(&text) else {
            continue;
        };
        let contact = Contact { text, url, remote };
        if !contacts.contains(&contact) {
            contacts.push(contact);
        }
    }
    let remotes: Vec<RemoteUrl> = contacts.iter().map(|c| c.url.clone()).collect();

    let inputs = Inputs {
        flag: profile_flag,
        env: env_profile.as_ref(),
        git_config: git_config_profile.as_ref(),
        remotes: &remotes,
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
    refuse_other_profiles_hosts(resolved.name, resolved.profile, &profiles, &remotes)?;
    let handled: Vec<Contact> = contacts
        .into_iter()
        .filter(|contact| resolved.profile.hosts.handles(&contact.url))
        .collect();
    if handled.is_empty() {
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
            match &reason {
                AwayReason::WrongKey { fingerprint } => {
                    wrong_key_note(resolved.name, resolved.profile, fingerprint);
                }
                AwayReason::NotPinned => eprintln!(
                    "gitaway: note: profile `{}` has no home host key yet, so it can't tell when \
                     you're home and uses the tunnel. Pin the key with `gitaway check {}`",
                    resolved.name, resolved.name
                ),
                _ => {}
            }
            Ok(Plan::Away(Box::new(AwayPlan {
                args,
                global_options: invocation.global_options.to_vec(),
                name: resolved.name.clone(),
                profile: resolved.profile.clone(),
                home: handled,
            })))
        }
        // No [home]: gitaway can't tell, so it always uses the tunnel.
        None => Ok(Plan::Away(Box::new(AwayPlan {
            args,
            global_options: invocation.global_options.to_vec(),
            name: resolved.name.clone(),
            profile: resolved.profile.clone(),
            home: handled,
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

/// A URL and the remote it belongs to, if any.
type RemoteAndUrl = (Option<String>, String);

/// What the repository says: `gitaway.profile`, and every URL the command
/// may contact.
fn query_repo(
    repo: &Repo,
    invocation: &Invocation<'_>,
) -> Result<(Option<String>, Vec<RemoteAndUrl>), RunError> {
    Ok((repo.profile()?, urls_of(repo, invocation)?))
}

/// Every URL the command may contact, as git will use them (after
/// `insteadOf`):
///
/// - `fetch --all`, `remote update` and the like: every remote's URLs;
/// - a URL in the arguments: that URL;
/// - a remote named in the arguments, else git's default remote for this
///   command: all of that remote's URLs (a push goes to each push URL);
/// - plus the submodules' URLs, for commands that can recurse into them.
///
/// Counting a URL git won't contact only costs an unneeded tunnel; missing
/// one would send a home host's traffic around the tunnel.
///
/// Each URL comes with the remote it belongs to, if any.
fn urls_of(repo: &Repo, invocation: &Invocation<'_>) -> Result<Vec<RemoteAndUrl>, RunError> {
    let names = repo.remote_names()?;
    let direction = invocation.direction();
    let of_remote = |name: &str| -> Result<Vec<RemoteAndUrl>, RunError> {
        Ok(repo
            .remote_urls(name, direction)?
            .into_iter()
            .map(|url| (Some(name.to_owned()), url))
            .collect())
    };
    let mut urls = Vec::new();
    if invocation.reaches_several_remotes() {
        for name in &names {
            urls.extend(of_remote(name)?);
        }
    } else {
        match invocation.remote_argument(|arg| names.iter().any(|name| name == arg)) {
            Some(RemoteArg::Url(url)) => urls.push((None, repo.expand_url(url)?)),
            Some(RemoteArg::Name(name)) => urls.extend(of_remote(name)?),
            None if invocation.subcommand == Some("clone") => {}
            None => urls.extend(of_remote(&repo.default_remote(direction)?)?),
        }
    }
    if invocation.reaches_submodules() {
        urls.extend(repo.submodule_urls()?.into_iter().map(|url| (None, url)));
    }
    Ok(urls)
}

/// Refuses a command that would contact another profile's home host. It
/// would run with `chosen`'s tunnel (or none), so that host's traffic would
/// go around its own tunnel.
fn refuse_other_profiles_hosts(
    chosen_name: &ProfileName,
    chosen: &Profile,
    profiles: &BTreeMap<ProfileName, Profile>,
    remotes: &[RemoteUrl],
) -> Result<(), Error> {
    for remote in remotes
        .iter()
        .filter(|remote| !chosen.hosts.handles(remote))
    {
        if let Some((other, _)) = profiles
            .iter()
            .find(|(name, profile)| *name != chosen_name && profile.hosts.handles(remote))
        {
            return Err(Error::msg(format!(
                "this command also reaches {}, a home host of profile `{other}`, but it runs \
                 with profile `{chosen_name}`. gitaway uses one tunnel per command: run it once \
                 per remote, such as `git push <remote>`",
                remote.host
            )));
        }
    }
    Ok(())
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
         If you are home, the server's key may have changed: `gitaway check {name}` fetches the \
         server's current key through the tunnel and offers to update it"
    );
}
