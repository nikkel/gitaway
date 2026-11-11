//! Find the git subcommand in a command line and decide whether it needs the
//! network.
//!
//! git accepts global options before the subcommand (`git -C dir push`), and
//! some of them take a separate value (`-C dir`, `-c key=value`). To find the
//! subcommand reliably, [`classify`] skips them using the same rules as git.
//!
//! Only network commands go through the home check (and, later, the tunnel).
//! Everything else is passed straight to git.
//!
//! # Extending
//!
//! - A new network command: add it to [`NETWORK_COMMANDS`]; to
//!   [`NETWORK_SUBCOMMANDS`] if only one of its subcommands talks to a remote
//!   (like `remote update`); or to [`NETWORK_WITH_OPTION`] if it only does
//!   with an option (like `remote add --fetch`).
//! - A new git global option: add it to [`GLOBAL_OPTIONS_WITH_VALUE`] if it can
//!   take its value as the next argument (`--opt value`). Options that only
//!   take `--opt=value`, or no value, need no change.

use std::ffi::{OsStr, OsString};

use super::remote_url;

/// Commands that always talk to a remote.
pub const NETWORK_COMMANDS: &[&str] = &["clone", "fetch", "pull", "push", "ls-remote"];

/// Commands where only some subcommands talk to a remote, as
/// `(command, subcommand)` pairs.
pub const NETWORK_SUBCOMMANDS: &[(&str, &str)] = &[
    ("remote", "update"),
    ("remote", "show"),
    ("remote", "prune"),
    ("submodule", "update"),
    ("submodule", "add"),
];

/// Commands that only talk to a remote when given one of some options, as
/// `(command, subcommand, options)`. An option matches as typed, or as
/// `--option=value`.
pub const NETWORK_WITH_OPTION: &[(&str, Option<&str>, &[&str])] = &[
    ("remote", Some("add"), &["-f", "--fetch"]),
    ("remote", Some("set-head"), &["-a", "--auto"]),
    ("archive", None, &["--remote"]),
];

/// git global options that can take their value as the next argument.
///
/// Taken from git's own option handling (`git.c`). Each also accepts the
/// `--opt=value` form, which needs no special handling here.
pub const GLOBAL_OPTIONS_WITH_VALUE: &[&str] = &[
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--config-env",
    "--attr-source",
];

/// Whether a git command talks to a remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
    /// Works on the local repository only, such as `commit` or `status`.
    Local,
    /// Talks to a remote, such as `push` or `fetch`.
    Network,
}

/// A git command line split into its parts.
///
/// The slices borrow from the arguments passed to [`classify`], so nothing is
/// copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Invocation<'a> {
    /// Global options before the subcommand, such as `-C dir`.
    pub global_options: &'a [OsString],
    /// The subcommand, such as `push`. `None` when there is none (`git
    /// --version`), when an unknown option comes first, or when it isn't
    /// valid UTF-8. git itself reports those cases.
    pub subcommand: Option<&'a str>,
    /// Everything after the subcommand.
    pub args: &'a [OsString],
    /// Whether the command talks to a remote.
    pub kind: Kind,
}

/// Whether a command fetches or pushes. git picks the remote, and the URL,
/// differently for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Direction {
    /// `fetch`, `pull`, `clone`, `ls-remote` and similar.
    Fetch,
    /// `push`.
    Push,
}

/// A remote named in a command's arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteArg<'a> {
    /// The name of a configured remote, as in `git push github`.
    Name(&'a str),
    /// A URL, as in `git clone ssh://git@192.0.2.10/app.git`.
    Url(&'a str),
}

impl<'a> Invocation<'a> {
    /// Whether the command pushes or fetches.
    pub fn direction(&self) -> Direction {
        match self.subcommand {
            Some("push") => Direction::Push,
            _ => Direction::Fetch,
        }
    }

    /// Whether the command fetches from several remotes rather than one:
    /// `fetch --all`, `fetch --multiple …`, the same for `pull`, and
    /// `remote update`. Every remote's URLs then count.
    pub fn reaches_several_remotes(&self) -> bool {
        match self.subcommand {
            Some("remote") => first_positional(self.args) == Some(OsStr::new("update")),
            Some("fetch" | "pull") => self
                .args
                .iter()
                .map_while(|arg| arg.to_str().filter(|&arg| arg != "--"))
                .any(|arg| arg == "--all" || arg == "--multiple"),
            _ => false,
        }
    }

    /// Whether the command may also contact the repository's submodules'
    /// servers: `submodule update`, and fetch, pull and push, which can
    /// recurse into submodules. `clone` and `ls-remote` don't use the
    /// current repository's submodules.
    pub fn reaches_submodules(&self) -> bool {
        matches!(
            self.subcommand,
            Some("fetch" | "pull" | "push" | "submodule")
        )
    }

    /// The first argument that's a configured remote's name or a network URL,
    /// such as `github` in `git push --force github main`.
    ///
    /// Options are skipped without knowing which take values. That's safe
    /// because a value is only picked if it's a real remote name or a valid
    /// URL. Arguments after `--` are paths, so the search stops there.
    pub fn remote_argument(&self, is_remote_name: impl Fn(&str) -> bool) -> Option<RemoteArg<'a>> {
        self.args
            .iter()
            .map_while(|arg| arg.to_str().filter(|&arg| arg != "--"))
            // `git archive --remote=<url>` names its remote inside an option.
            .filter_map(|arg| match arg.strip_prefix("--remote=") {
                Some(value) => Some(value),
                None if arg.starts_with('-') => None,
                None => Some(arg),
            })
            .find_map(|arg| {
                if is_remote_name(arg) {
                    Some(RemoteArg::Name(arg))
                } else {
                    remote_url::parse(arg).map(|_| RemoteArg::Url(arg))
                }
            })
    }
}

/// Splits git arguments (without the program name) and classifies the command.
///
/// # Examples
///
/// ```
/// use std::ffi::OsString;
/// use gitaway::git::classify::{classify, Kind};
///
/// let args: Vec<OsString> = ["-C", "app", "push", "origin"].map(OsString::from).into();
/// let invocation = classify(&args);
///
/// assert_eq!(invocation.subcommand, Some("push"));
/// assert_eq!(invocation.kind, Kind::Network);
/// assert_eq!(invocation.global_options.len(), 2);
/// ```
pub fn classify(args: &[OsString]) -> Invocation<'_> {
    let Some(index) = subcommand_index(args) else {
        return Invocation {
            global_options: args,
            subcommand: None,
            args: &[],
            kind: Kind::Local,
        };
    };

    let subcommand = args[index].to_str();
    let rest = &args[index + 1..];
    let kind = match subcommand {
        Some(command) if is_network(command, rest) => Kind::Network,
        _ => Kind::Local,
    };

    Invocation {
        global_options: &args[..index],
        subcommand,
        args: rest,
        kind,
    }
}

/// Returns the index of the subcommand, or `None` if git's global options run
/// out without one, or an unknown option comes first.
fn subcommand_index(args: &[OsString]) -> Option<usize> {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        let Some(arg) = arg.to_str() else {
            // Non-UTF-8 can't be a known option. Treat it as the subcommand
            // position so git can report it.
            return Some(index);
        };
        if !arg.starts_with('-') {
            return Some(index);
        }
        if GLOBAL_OPTIONS_WITH_VALUE.contains(&arg) {
            index += 2;
        } else if is_known_global_flag(arg) {
            index += 1;
        } else {
            return None;
        }
    }
    None
}

/// Global options that take no separate value. An option written as
/// `--opt=value` also counts, because its value is part of the same argument.
fn is_known_global_flag(arg: &str) -> bool {
    const FLAGS: &[&str] = &[
        "-p",
        "--paginate",
        "-P",
        "--no-pager",
        "--bare",
        "--no-replace-objects",
        "--no-lazy-fetch",
        "--no-optional-locks",
        "--no-advice",
        "--literal-pathspecs",
        "--glob-pathspecs",
        "--noglob-pathspecs",
        "--icase-pathspecs",
        "--exec-path",
        "--html-path",
        "--man-path",
        "--info-path",
        "-v",
        "--version",
        "-h",
        "--help",
    ];
    let name = arg.split_once('=').map_or(arg, |(name, _)| name);
    FLAGS.contains(&name) || (arg.contains('=') && GLOBAL_OPTIONS_WITH_VALUE.contains(&name))
}

fn is_network(command: &str, rest: &[OsString]) -> bool {
    if NETWORK_COMMANDS.contains(&command) {
        return true;
    }
    let sub = first_positional(rest);
    let with_option = NETWORK_WITH_OPTION.iter().any(|&(c, s, options)| {
        c == command
            && s.is_none_or(|s| sub == Some(OsStr::new(s)))
            && rest.iter().any(|arg| has_option(arg, options))
    });
    with_option
        || sub.is_some_and(|sub| {
            NETWORK_SUBCOMMANDS
                .iter()
                .any(|&(c, s)| c == command && OsStr::new(s) == sub)
        })
}

/// Whether `arg` is one of `options`, as typed or as `--option=value`.
fn has_option(arg: &OsStr, options: &[&str]) -> bool {
    let Some(arg) = arg.to_str() else {
        return false;
    };
    options.iter().any(|&option| {
        arg == option
            || arg
                .strip_prefix(option)
                .is_some_and(|rest| rest.starts_with('='))
    })
}

/// The first argument that isn't an option, as in `git remote -v update`.
fn first_positional(args: &[OsString]) -> Option<&OsStr> {
    args.iter()
        .map(OsString::as_os_str)
        .find(|arg| !arg.as_encoded_bytes().starts_with(b"-"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn network_commands_are_network() {
        for command in ["clone", "fetch", "pull", "push", "ls-remote"] {
            let args = os(&[command]);
            assert_eq!(classify(&args).kind, Kind::Network, "{command}");
        }
    }

    #[test]
    fn local_commands_are_local() {
        for command in ["commit", "status", "log", "remote", "submodule", "rebase"] {
            let args = os(&[command]);
            assert_eq!(classify(&args).kind, Kind::Local, "{command}");
        }
    }

    #[test]
    fn network_subcommands_are_found_past_options() {
        let args = os(&["remote", "-v", "update"]);
        assert_eq!(classify(&args).kind, Kind::Network);

        let args = os(&["submodule", "--quiet", "update", "--init"]);
        assert_eq!(classify(&args).kind, Kind::Network);
    }

    #[test]
    fn other_subcommands_of_mixed_commands_are_local() {
        let args = os(&["remote", "add", "update", "ssh://example.com/update.git"]);
        assert_eq!(classify(&args).kind, Kind::Local);

        let args = os(&["submodule", "status"]);
        assert_eq!(classify(&args).kind, Kind::Local);
    }

    #[test]
    fn global_options_with_separate_values_are_skipped() {
        let args = os(&["-C", "push", "-c", "user.name=alice", "status"]);
        let invocation = classify(&args);
        assert_eq!(invocation.subcommand, Some("status"));
        assert_eq!(invocation.kind, Kind::Local);
        assert_eq!(invocation.global_options, &args[..4]);
    }

    #[test]
    fn global_options_with_inline_values_are_skipped() {
        let args = os(&["--git-dir=x", "--work-tree=y", "--no-pager", "fetch"]);
        let invocation = classify(&args);
        assert_eq!(invocation.subcommand, Some("fetch"));
        assert_eq!(invocation.kind, Kind::Network);
    }

    #[test]
    fn arguments_after_the_subcommand_are_returned() {
        let args = os(&["-P", "push", "--force", "origin", "main"]);
        let invocation = classify(&args);
        assert_eq!(invocation.args, &args[2..]);
    }

    #[test]
    fn no_subcommand_is_local() {
        for args in [os(&[]), os(&["--version"]), os(&["-C", "dir"])] {
            let invocation = classify(&args);
            assert_eq!(invocation.subcommand, None);
            assert_eq!(invocation.kind, Kind::Local);
            assert!(invocation.args.is_empty());
        }
    }

    #[test]
    fn unknown_leading_option_stops_classification() {
        let args = os(&["--not-a-git-option", "push"]);
        let invocation = classify(&args);
        assert_eq!(invocation.subcommand, None);
        assert_eq!(invocation.kind, Kind::Local);
    }

    #[test]
    fn a_value_that_looks_like_a_command_is_not_the_subcommand() {
        let args = os(&["-c", "push", "log"]);
        assert_eq!(classify(&args).subcommand, Some("log"));
    }

    #[test]
    fn direction() {
        assert_eq!(classify(&os(&["push"])).direction(), Direction::Push);
        for command in ["fetch", "pull", "clone", "ls-remote"] {
            assert_eq!(classify(&os(&[command])).direction(), Direction::Fetch);
        }
    }

    #[test]
    fn other_commands_that_contact_a_remote_are_network_commands() {
        let network = |args: &[&str]| classify(&os(args)).kind == Kind::Network;
        assert!(network(&[
            "submodule",
            "add",
            "ssh://git@192.0.2.10/lib.git",
            "lib"
        ]));
        assert!(network(&["remote", "show", "origin"]));
        assert!(network(&["remote", "prune", "origin"]));
        assert!(network(&["remote", "set-head", "origin", "--auto"]));
        assert!(network(&["remote", "set-head", "origin", "-a"]));
        assert!(network(&[
            "remote",
            "add",
            "-f",
            "home",
            "ssh://git@192.0.2.10/app.git"
        ]));
        assert!(network(&[
            "remote",
            "add",
            "--fetch",
            "home",
            "ssh://git@192.0.2.10/app.git"
        ]));
        assert!(network(&[
            "archive",
            "--remote=ssh://git@192.0.2.10/app.git",
            "HEAD"
        ]));
        assert!(network(&[
            "archive",
            "--remote",
            "ssh://git@192.0.2.10/app.git",
            "HEAD"
        ]));
        // The same commands without the part that contacts a remote stay local.
        assert!(!network(&["remote", "set-head", "origin", "main"]));
        assert!(!network(&[
            "remote",
            "add",
            "home",
            "ssh://git@192.0.2.10/app.git"
        ]));
        assert!(!network(&["archive", "HEAD"]));
        assert!(!network(&["remote", "-v"]));
    }

    #[test]
    fn an_archive_remote_given_with_equals_is_found() {
        let args = os(&["archive", "--remote=ssh://git@192.0.2.10/app.git", "HEAD"]);
        assert_eq!(
            classify(&args).remote_argument(|_| false),
            Some(RemoteArg::Url("ssh://git@192.0.2.10/app.git"))
        );
    }

    #[test]
    fn only_remote_update_reaches_every_remote() {
        let several = |args: &[&str]| classify(&os(args)).reaches_several_remotes();
        assert!(several(&["remote", "update"]));
        assert!(!several(&["remote", "show", "origin"]));
        assert!(!several(&["remote", "prune", "origin"]));
    }

    #[test]
    fn commands_reaching_several_remotes_or_submodules_are_recognised() {
        let several = |args: &[&str]| classify(&os(args)).reaches_several_remotes();
        assert!(several(&["fetch", "--all"]));
        assert!(several(&["fetch", "--multiple", "origin", "home"]));
        assert!(several(&["pull", "--all"]));
        assert!(several(&["remote", "update"]));
        assert!(!several(&["fetch", "origin"]));
        // For push, --all means every branch, not every remote.
        assert!(!several(&["push", "--all"]));
        assert!(!several(&["fetch", "--", "--all"]));

        let submodules = |args: &[&str]| classify(&os(args)).reaches_submodules();
        assert!(submodules(&["submodule", "update", "--init"]));
        assert!(submodules(&["push"]));
        assert!(!submodules(&["clone", "ssh://git@192.0.2.10/app.git"]));
        assert!(!submodules(&["ls-remote", "origin"]));
    }

    #[test]
    fn remote_argument_finds_named_remotes_and_urls() {
        let is_remote = |name: &str| ["origin", "github"].contains(&name);
        let remote_of = |args: &[&str]| {
            let args = os(args);
            classify(&args)
                .remote_argument(is_remote)
                .map(|r| format!("{r:?}"))
        };

        assert_eq!(
            remote_of(&["push", "--force", "github", "main"]),
            Some("Name(\"github\")".into())
        );
        assert_eq!(
            remote_of(&["fetch", "--depth", "1", "origin"]),
            Some("Name(\"origin\")".into())
        );
        assert_eq!(
            remote_of(&[
                "clone",
                "--branch",
                "main",
                "ssh://git@192.0.2.10/a.git",
                "dir"
            ]),
            Some("Url(\"ssh://git@192.0.2.10/a.git\")".into())
        );
        assert_eq!(
            remote_of(&["push", "git@example.com:a.git", "main"]),
            Some("Url(\"git@example.com:a.git\")".into())
        );
        assert_eq!(remote_of(&["push"]), None);
        assert_eq!(
            remote_of(&["push", "main"]),
            None,
            "a branch isn't a remote"
        );
        assert_eq!(
            remote_of(&["pull", "--", "origin"]),
            None,
            "after -- are paths"
        );
        assert_eq!(
            remote_of(&["-C", "origin", "fetch"]),
            None,
            "global option values aren't args"
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_subcommand_is_passed_through_as_local() {
        use std::os::unix::ffi::OsStringExt;
        let args = vec![OsString::from("-P"), OsString::from_vec(vec![0xff, b'x'])];
        let invocation = classify(&args);
        assert_eq!(invocation.subcommand, None);
        assert_eq!(invocation.kind, Kind::Local);
        assert_eq!(invocation.global_options, &args[..1]);
    }
}
