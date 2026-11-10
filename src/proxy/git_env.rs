//! The environment that points one git command at the proxy.
//!
//! Settings go to git through its environment (`GIT_CONFIG_COUNT`,
//! `GIT_CONFIG_KEY_n`, `GIT_CONFIG_VALUE_n`, supported since git 2.31), not
//! its command line, because other users can read a process's command line
//! but not its environment.
//!
//! | Setting | For |
//! |---|---|
//! | `http.proxy=socks5h://gitaway:<token>@127.0.0.1:<port>` | `http://` and `https://` |
//! | `credential.socks5h://127.0.0.1:<port>.helper=` (empty) | stops git saving that throwaway proxy password in your credential helper |
//! | `http.<home URL>.proxy`, `remote.<its remote>.proxy` (same proxy) | outranks your own proxy settings for home URLs (see below) |
//! | `GIT_SSH_COMMAND=<your ssh command> -o ProxyCommand=… %h %p %n` | `ssh://` and `user@host:path` |
//!
//! Your own ssh command is kept: `GIT_SSH_COMMAND`, else `core.sshCommand`,
//! else `GIT_SSH`, else `ssh`, with the `ProxyCommand` option added. Options
//! on ssh's command line win over `~/.ssh/config`, so a `ProxyJump` there
//! doesn't bypass the tunnel. `NO_PROXY` and `no_proxy` are removed, so they
//! can't send a home host around the proxy.
//!
//! # Your proxy settings
//!
//! git picks the most specific proxy setting: `remote.<name>.proxy` beats
//! `http.<url>.proxy`, which beats `http.proxy`. A setting of your own for a
//! home URL would take it around the tunnel, so gitaway sets those two for
//! each home URL too, for this command only. git reads these settings last,
//! so for the same key they win. Nothing in your config files changes, and
//! URLs that aren't home keep your settings.
//!
//! ssh has no such order: it keeps the first `ProxyCommand` or `ProxyJump`
//! it's given, and yours come before gitaway's. [`check_ssh`] refuses a
//! command whose ssh command sets one, and says how to move it to
//! `~/.ssh/config`, which gitaway's option does outrank.
//!
//! `git://` isn't covered: git runs `core.gitProxy` as a bare program with
//! no arguments of its own, so it can't start the helper. The protocol is
//! unauthenticated and Forgejo and Gitea don't serve it.

use std::net::SocketAddr;
use std::path::Path;

use super::helper::COMMAND;
use super::socks::USERNAME;
use super::{PROXY_ENV, TOKEN_ENV};

/// Environment changes for git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitEnv {
    /// Variables to set.
    pub set: Vec<(String, String)>,
    /// Variables to remove.
    pub remove: Vec<&'static str>,
}

/// The user's ssh setup, as git would use it.
#[derive(Debug, Clone, Copy, Default)]
pub struct SshSettings<'a> {
    /// `GIT_SSH_COMMAND`, if set.
    pub git_ssh_command: Option<&'a str>,
    /// `git config core.sshCommand`, if set.
    pub core_ssh_command: Option<&'a str>,
    /// `GIT_SSH`, if set: a program, without arguments.
    pub git_ssh: Option<&'a str>,
}

/// The home URLs a command reaches over HTTP, so git's own proxy settings
/// for them can be outranked.
#[derive(Debug, Clone, Copy, Default)]
pub struct HomeHttp<'a> {
    /// The `http://` and `https://` home URLs, exactly as git uses them.
    pub urls: &'a [&'a str],
    /// The remotes those URLs belong to.
    pub remotes: &'a [&'a str],
}

/// The environment for a git command that should use the proxy at
/// `proxy`. `exe` is this gitaway program (for the helper), `home` the
/// command's home URLs over HTTP, and `existing_count` the current
/// `GIT_CONFIG_COUNT`, if any.
pub fn build(
    proxy: SocketAddr,
    token: &str,
    exe: &Path,
    ssh: SshSettings<'_>,
    home: HomeHttp<'_>,
    existing_count: Option<&str>,
) -> Result<GitEnv, String> {
    let exe = exe
        .to_str()
        .ok_or_else(|| format!("gitaway's path isn't valid UTF-8: {}", exe.display()))?;
    let helper = format!("{} {COMMAND}", shell_quote(exe));
    let proxy_url = format!("socks5h://{USERNAME}:{token}@{proxy}");

    let mut config = vec![
        ("http.proxy".to_owned(), proxy_url.clone()),
        (
            format!("credential.socks5h://{proxy}.helper"),
            String::new(),
        ),
    ];
    // The whole URL is as specific as a key can be for it, and on a tie the
    // value git reads last (this one) wins.
    for url in home.urls {
        config.push((format!("http.{url}.proxy"), proxy_url.clone()));
    }
    for remote in home.remotes {
        config.push((format!("remote.{remote}.proxy"), proxy_url.clone()));
    }
    let first = match existing_count.map(str::trim) {
        None | Some("") => 0,
        Some(count) => count
            .parse::<usize>()
            .map_err(|_| format!("GIT_CONFIG_COUNT is `{count}`, not a number"))?,
    };

    let mut set = Vec::new();
    for (i, (key, value)) in config.iter().cloned().enumerate() {
        set.push((format!("GIT_CONFIG_KEY_{}", first + i), key));
        set.push((format!("GIT_CONFIG_VALUE_{}", first + i), value));
    }
    set.push((
        "GIT_CONFIG_COUNT".to_owned(),
        (first + config.len()).to_string(),
    ));

    // %h is the host after ~/.ssh/config (an alias's Hostname), %n the name
    // ssh was given: the helper needs both to recognise a home host.
    let proxy_command = format!("ProxyCommand={helper} %h %p %n");
    set.push((
        "GIT_SSH_COMMAND".to_owned(),
        format!("{} -o {}", ssh_base(ssh), shell_quote(&proxy_command)),
    ));
    set.push((PROXY_ENV.to_owned(), proxy.to_string()));
    set.push((TOKEN_ENV.to_owned(), token.to_owned()));

    Ok(GitEnv {
        set,
        remove: vec!["NO_PROXY", "no_proxy"],
    })
}

/// Refuses an ssh command that sets its own `ProxyCommand` or `ProxyJump`
/// (`-J`): ssh keeps the first one it's given, so it would take `host`
/// around the tunnel. The error says how to move it to `~/.ssh/config`.
pub fn check_ssh(ssh: SshSettings<'_>, host: &str) -> Result<(), String> {
    let (setting, command) = match (
        non_empty(ssh.git_ssh_command),
        non_empty(ssh.core_ssh_command),
    ) {
        (Some(command), _) => ("GIT_SSH_COMMAND", command),
        (None, Some(command)) => ("core.sshCommand", command),
        // GIT_SSH is a program without options; there's nothing to read.
        (None, None) => return Ok(()),
    };
    let Some(option) = proxy_option(command) else {
        return Ok(());
    };
    Err(format!(
        "your ssh command ({setting}) sets {option}. ssh uses the first proxy it's given, so it \
         would take {host} around gitaway's tunnel. Move it to ~/.ssh/config, where gitaway \
         outranks it for home hosts while you're away and it still applies everywhere else:\n\n    \
         Host <the hosts that need it>\n        {option} <its value>\n\n\
         then remove it from {setting}."
    ))
}

/// The proxy option in an ssh command line, if it sets one.
fn proxy_option(command: &str) -> Option<&'static str> {
    let mut words = command
        .split_whitespace()
        .map(|word| word.trim_matches(['\'', '"']));
    while let Some(word) = words.next() {
        if word.starts_with("-J") {
            return Some("ProxyJump");
        }
        let option = match word.strip_prefix("-o") {
            Some("") => words.next().unwrap_or_default(),
            Some(rest) => rest,
            None => continue,
        }
        .to_ascii_lowercase();
        if option.starts_with("proxycommand") {
            return Some("ProxyCommand");
        }
        if option.starts_with("proxyjump") {
            return Some("ProxyJump");
        }
    }
    None
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !v.trim().is_empty())
}

/// The ssh command git would run, as a shell command line.
fn ssh_base(ssh: SshSettings<'_>) -> String {
    if let Some(command) = non_empty(ssh.git_ssh_command) {
        return command.to_owned();
    }
    if let Some(command) = non_empty(ssh.core_ssh_command) {
        return command.to_owned();
    }
    if let Some(program) = non_empty(ssh.git_ssh) {
        return shell_quote(program);
    }
    "ssh".to_owned()
}

/// Quotes `s` as one word for a POSIX shell. git runs `GIT_SSH_COMMAND`
/// through a shell, and ssh runs `ProxyCommand`
/// through one.
pub fn shell_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_.,:=@%+".contains(&b));
    if plain {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy() -> SocketAddr {
        "127.0.0.1:41234".parse().unwrap()
    }

    fn value<'a>(env: &'a GitEnv, name: &str) -> &'a str {
        env.set
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("{name} not set"))
    }

    /// The config entries as key = value, in order.
    fn config(env: &GitEnv) -> Vec<(String, String)> {
        let count: usize = value(env, "GIT_CONFIG_COUNT").parse().unwrap();
        (0..count)
            .filter(|i| {
                env.set
                    .iter()
                    .any(|(k, _)| k == &format!("GIT_CONFIG_KEY_{i}"))
            })
            .map(|i| {
                (
                    value(env, &format!("GIT_CONFIG_KEY_{i}")).to_owned(),
                    value(env, &format!("GIT_CONFIG_VALUE_{i}")).to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn git_gets_the_proxy_for_http_and_ssh() {
        let env = build(
            proxy(),
            "tok",
            Path::new("/usr/local/bin/gitaway"),
            SshSettings::default(),
            HomeHttp::default(),
            None,
        )
        .unwrap();
        assert_eq!(
            config(&env),
            [
                (
                    "http.proxy".into(),
                    "socks5h://gitaway:tok@127.0.0.1:41234".into()
                ),
                (
                    "credential.socks5h://127.0.0.1:41234.helper".into(),
                    String::new()
                ),
            ]
        );
        assert_eq!(
            value(&env, "GIT_SSH_COMMAND"),
            "ssh -o 'ProxyCommand=/usr/local/bin/gitaway __proxy-connect %h %p %n'"
        );
        assert_eq!(value(&env, PROXY_ENV), "127.0.0.1:41234");
        assert_eq!(value(&env, TOKEN_ENV), "tok");
        assert_eq!(env.remove, ["NO_PROXY", "no_proxy"]);
    }

    #[test]
    fn home_urls_and_their_remotes_get_the_proxy_too() {
        let home = HomeHttp {
            urls: &["https://git.example.com/alice/app.git"],
            remotes: &["origin"],
        };
        let env = build(
            proxy(),
            "tok",
            Path::new("/g"),
            SshSettings::default(),
            home,
            None,
        )
        .unwrap();
        let proxy_url = "socks5h://gitaway:tok@127.0.0.1:41234".to_owned();
        let config = config(&env);
        assert!(config.contains(&(
            "http.https://git.example.com/alice/app.git.proxy".into(),
            proxy_url.clone()
        )));
        assert!(config.contains(&("remote.origin.proxy".into(), proxy_url)));
    }

    #[test]
    fn an_ssh_command_with_its_own_proxy_is_refused() {
        let check = |git_ssh_command, core_ssh_command| {
            check_ssh(
                SshSettings {
                    git_ssh_command,
                    core_ssh_command,
                    git_ssh: None,
                },
                "git.example.com",
            )
        };
        for command in [
            "ssh -J bastion.example.net",
            "ssh -Jbastion.example.net",
            "ssh -o ProxyJump=bastion.example.net",
            "ssh -oProxyCommand='nc %h %p'",
            "ssh -o 'proxycommand nc %h %p'",
        ] {
            let error = check(Some(command), None).unwrap_err();
            assert!(error.contains("GIT_SSH_COMMAND"), "{command}: {error}");
            assert!(error.contains("~/.ssh/config"), "{command}: {error}");
        }
        assert!(
            check(None, Some("ssh -J b"))
                .unwrap_err()
                .contains("core.sshCommand")
        );
        assert!(check(Some("ssh -i ~/.ssh/id_work -o IdentitiesOnly=yes"), None).is_ok());
        assert!(check(None, None).is_ok());
        // GIT_SSH_COMMAND wins over core.sshCommand, as in git.
        assert!(check(Some("ssh"), Some("ssh -J b")).is_ok());
    }

    #[test]
    fn existing_config_from_the_environment_is_kept() {
        let env = build(
            proxy(),
            "tok",
            Path::new("/g"),
            SshSettings::default(),
            HomeHttp::default(),
            Some("2"),
        )
        .unwrap();
        assert_eq!(value(&env, "GIT_CONFIG_COUNT"), "4");
        assert_eq!(value(&env, "GIT_CONFIG_KEY_2"), "http.proxy");
        assert!(
            !env.set.iter().any(|(k, _)| k == "GIT_CONFIG_KEY_0"),
            "the user's own are untouched"
        );
        assert!(
            build(
                proxy(),
                "tok",
                Path::new("/g"),
                SshSettings::default(),
                HomeHttp::default(),
                Some("x")
            )
            .is_err()
        );
    }

    #[test]
    fn the_users_ssh_command_is_kept_in_gits_order() {
        let ssh = |git_ssh_command, core_ssh_command, git_ssh| {
            let settings = SshSettings {
                git_ssh_command,
                core_ssh_command,
                git_ssh,
            };
            let env = build(
                proxy(),
                "t",
                Path::new("/g"),
                settings,
                HomeHttp::default(),
                None,
            )
            .unwrap();
            value(&env, "GIT_SSH_COMMAND")
                .split(" -o 'ProxyCommand")
                .next()
                .unwrap()
                .to_owned()
        };
        assert_eq!(
            ssh(Some("ssh -i ~/.ssh/id_work"), Some("ssh -v"), Some("/x")),
            "ssh -i ~/.ssh/id_work"
        );
        assert_eq!(ssh(None, Some("ssh -v"), Some("/x")), "ssh -v");
        assert_eq!(ssh(None, None, Some("/opt/my ssh")), "'/opt/my ssh'");
        assert_eq!(ssh(Some(""), None, None), "ssh");
    }

    // git runs ssh commands through a POSIX shell, which Windows lacks.
    #[cfg(unix)]
    #[test]
    fn paths_with_spaces_and_quotes_survive_two_shells() {
        let env = build(
            proxy(),
            "t",
            Path::new("/Users/alice/My Tools/it's/gitaway"),
            SshSettings::default(),
            HomeHttp::default(),
            None,
        )
        .unwrap();
        // What ssh receives after git's shell, and what ssh's shell runs.
        let command = value(&env, "GIT_SSH_COMMAND");
        let after_git_shell = run_sh(&format!(
            "printf %s {}",
            command.split_once(" -o ").unwrap().1
        ));
        assert_eq!(
            after_git_shell,
            r"ProxyCommand='/Users/alice/My Tools/it'\''s/gitaway' __proxy-connect %h %p %n"
        );
        let program = after_git_shell
            .strip_prefix("ProxyCommand=")
            .unwrap()
            .strip_suffix(" __proxy-connect %h %p %n")
            .unwrap();
        assert_eq!(
            run_sh(&format!("printf %s {program}")),
            "/Users/alice/My Tools/it's/gitaway"
        );
    }

    #[cfg(unix)]
    fn run_sh(script: &str) -> String {
        let out = std::process::Command::new("sh")
            .args(["-c", script])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    }

    #[test]
    fn shell_quoting() {
        assert_eq!(shell_quote("ssh"), "ssh");
        assert_eq!(shell_quote("/usr/bin/ssh"), "/usr/bin/ssh");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("$(rm -rf /)"), "'$(rm -rf /)'");
    }
}
