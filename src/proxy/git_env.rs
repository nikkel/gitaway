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
//! | `GIT_SSH_COMMAND=<your ssh command> -o ProxyCommand=…` | `ssh://` and `user@host:path` |
//!
//! Your own ssh command is kept: `GIT_SSH_COMMAND`, else `core.sshCommand`,
//! else `GIT_SSH`, else `ssh`, with the `ProxyCommand` option added. Options
//! on ssh's command line win over `~/.ssh/config`, so a `ProxyJump` there
//! doesn't bypass the tunnel. `NO_PROXY` and `no_proxy` are removed, so they
//! can't send a home host around the proxy.
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

/// The environment for a git command that should use the proxy at
/// `proxy`. `exe` is this gitaway program (for the helper), and
/// `existing_count` the current `GIT_CONFIG_COUNT`, if any.
pub fn build(
    proxy: SocketAddr,
    token: &str,
    exe: &Path,
    ssh: SshSettings<'_>,
    existing_count: Option<&str>,
) -> Result<GitEnv, String> {
    let exe = exe
        .to_str()
        .ok_or_else(|| format!("gitaway's path isn't valid UTF-8: {}", exe.display()))?;
    let helper = format!("{} {COMMAND}", shell_quote(exe));
    let proxy_url = format!("socks5h://{USERNAME}:{token}@{proxy}");

    let config = [
        ("http.proxy".to_owned(), proxy_url),
        (
            format!("credential.socks5h://{proxy}.helper"),
            String::new(),
        ),
    ];
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

    let proxy_command = format!("ProxyCommand={helper} %h %p");
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

/// The ssh command git would run, as a shell command line.
fn ssh_base(ssh: SshSettings<'_>) -> String {
    fn non_empty(value: Option<&str>) -> Option<&str> {
        value.filter(|v| !v.trim().is_empty())
    }
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
            "ssh -o 'ProxyCommand=/usr/local/bin/gitaway __proxy-connect %h %p'"
        );
        assert_eq!(value(&env, PROXY_ENV), "127.0.0.1:41234");
        assert_eq!(value(&env, TOKEN_ENV), "tok");
        assert_eq!(env.remove, ["NO_PROXY", "no_proxy"]);
    }

    #[test]
    fn existing_config_from_the_environment_is_kept() {
        let env = build(
            proxy(),
            "tok",
            Path::new("/g"),
            SshSettings::default(),
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
            let env = build(proxy(), "t", Path::new("/g"), settings, None).unwrap();
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
            r"ProxyCommand='/Users/alice/My Tools/it'\''s/gitaway' __proxy-connect %h %p"
        );
        let program = after_git_shell
            .strip_prefix("ProxyCommand=")
            .unwrap()
            .strip_suffix(" __proxy-connect %h %p")
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
