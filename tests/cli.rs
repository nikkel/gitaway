//! End-to-end tests: run the real `gitaway` binary.
//!
//! Each test gets its own temporary home and config directory, with the
//! user's git config and real SSH kept out (`GIT_CONFIG_GLOBAL`,
//! `GIT_SSH_COMMAND=false`). On Unix, `git` on `PATH` is a wrapper that logs
//! each command line and then runs the real git, so tests can check exactly
//! which git commands gitaway ran, or that it ran none.

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

const SERVER_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

struct Env {
    root: TempDir,
}

impl Env {
    fn new() -> Self {
        let env = Self {
            root: TempDir::new().unwrap(),
        };
        std::fs::create_dir_all(env.path("bin")).unwrap();
        #[cfg(unix)]
        env.install_logging_git();
        env
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }

    /// `gitaway` with an isolated environment.
    fn gitaway(&self) -> Command {
        let mut cmd = Command::cargo_bin("gitaway").unwrap();
        let path = format!(
            "{}:{}",
            self.path("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        cmd.env("HOME", self.root.path())
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("GIT_CONFIG_GLOBAL", self.path("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_SSH_COMMAND", "false")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GITAWAY_LOG", self.path("git.log"))
            .env("PATH", path)
            .env_remove("GITAWAY_PROFILE")
            .current_dir(self.root.path());
        cmd
    }

    /// Real git in `dir`, isolated the same way.
    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = StdCommand::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", self.path("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// A new repository at `name` with `origin` set to `url`.
    fn repo(&self, name: &str, origin: &str) -> PathBuf {
        let dir = self.path(name);
        std::fs::create_dir_all(&dir).unwrap();
        self.git(&dir, &["init", "--quiet", "--initial-branch=main"]);
        self.git(&dir, &["remote", "add", "origin", origin]);
        dir
    }

    /// The git command lines gitaway ran, one per entry.
    fn git_log(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("git.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[cfg(unix)]
    fn install_logging_git(&self) {
        use std::os::unix::fs::PermissionsExt;
        let real = String::from_utf8(
            StdCommand::new("sh")
                .args(["-c", "command -v git"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        let script =
            format!("#!/bin/sh\necho \"$*\" >> \"$GITAWAY_LOG\"\nexec \"{real}\" \"$@\"\n");
        let path = self.path("bin/git");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Creates a profile with `gitaway profile add`, using flags only.
    fn add_profile(&self, name: &str, extra: &[&str]) {
        self.gitaway()
            .args([
                "profile",
                "add",
                name,
                "--endpoint",
                "vpn.example.net:51820",
                "--server-public-key",
                SERVER_KEY,
            ])
            .args(extra)
            .assert()
            .success();
    }
}

mod passthrough {
    use super::*;

    #[test]
    fn help_and_version() {
        let env = Env::new();
        env.gitaway()
            .assert()
            .success()
            .stdout(predicate::str::contains("gitaway's own commands"));
        env.gitaway()
            .arg("--help")
            .assert()
            .success()
            .stdout(predicate::str::contains("Usage"));
        env.gitaway()
            .arg("--version")
            .assert()
            .success()
            .stdout(predicate::str::starts_with("gitaway "));
    }

    #[test]
    fn reserved_commands_are_errors() {
        let env = Env::new();
        for command in ["list", "tunnel", "peer", "key"] {
            env.gitaway()
                .arg(command)
                .assert()
                .code(1)
                .stderr(predicate::str::contains("isn't available yet"));
        }
    }

    #[test]
    fn gits_exit_code_is_returned() {
        let env = Env::new();
        let repo = env.repo("app", "https://example.com/app.git");
        env.gitaway()
            .args([
                "-C",
                repo.to_str().unwrap(),
                "rev-parse",
                "--verify",
                "no-such-ref",
            ])
            .assert()
            .code(128);
        env.gitaway()
            .args([
                "-C",
                repo.to_str().unwrap(),
                "rev-parse",
                "--is-inside-work-tree",
            ])
            .assert()
            .success()
            .stdout("true\n");
    }

    #[cfg(unix)]
    #[test]
    fn local_commands_reach_git_unchanged() {
        let env = Env::new();
        env.repo("app", "https://example.com/app.git");
        env.gitaway()
            .args([
                "-C",
                "app",
                "-c",
                "core.quotepath=off",
                "log",
                "--format=%s %b",
                "-n",
                "1",
                "--",
                "a b",
            ])
            .assert()
            // An empty repository has no log: git exits 128.
            .code(128);
        assert_eq!(
            env.git_log(),
            ["-C app -c core.quotepath=off log --format=%s %b -n 1 -- a b"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn without_profiles_network_commands_run_directly() {
        let env = Env::new();
        env.repo("app", "ssh://git@192.0.2.10/app.git");
        env.gitaway()
            .args(["-C", "app", "ls-remote", "origin"])
            .assert()
            .code(128);
        // No queries: with no profiles there's nothing to look up.
        assert_eq!(env.git_log(), ["-C app ls-remote origin"]);
    }
}

mod profiles {
    use super::*;

    #[test]
    fn add_list_show_default_remove() {
        let env = Env::new();
        env.add_profile(
            "home",
            &[
                "--home-ssh",
                "192.0.2.10:2222",
                "--host-key",
                HOST_KEY,
                "--host",
                "192.0.2.10=10.0.0.1",
                "--default",
            ],
        );
        env.add_profile("office", &[]);

        env.gitaway()
            .args(["profile", "list"])
            .assert()
            .success()
            .stdout(predicate::str::contains("* home").and(predicate::str::contains("  office")));
        env.gitaway()
            .args(["profile", "show", "home"])
            .assert()
            .success()
            .stdout(
                predicate::str::contains("ssh = \"192.0.2.10:2222\"")
                    .and(predicate::str::contains("SHA256:")),
            );

        env.gitaway()
            .args(["profile", "default", "office"])
            .assert()
            .success();
        env.gitaway()
            .args(["profile", "list"])
            .assert()
            .stdout(predicate::str::contains("* office"));

        env.gitaway()
            .args(["profile", "remove", "office", "--yes"])
            .assert()
            .success();
        env.gitaway()
            .args(["profile", "list"])
            .assert()
            .stdout(predicate::str::contains("office").not());
        let config = std::fs::read_to_string(env.path("config/gitaway/config.toml")).unwrap();
        assert!(!config.contains("office"), "removing the default clears it");
    }

    #[test]
    fn add_without_required_flags_fails_outside_a_terminal() {
        let env = Env::new();
        env.gitaway()
            .args(["profile", "add", "home"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("--endpoint"));
        env.gitaway()
            .args([
                "profile",
                "add",
                "../evil",
                "--endpoint",
                "vpn.example.net:1",
                "--server-public-key",
                SERVER_KEY,
            ])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("invalid profile name"));
    }

    #[test]
    fn adding_twice_fails() {
        let env = Env::new();
        env.add_profile("home", &[]);
        env.gitaway()
            .args([
                "profile",
                "add",
                "home",
                "--endpoint",
                "vpn.example.net:1",
                "--server-public-key",
                SERVER_KEY,
            ])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("already exists"));
    }

    #[test]
    fn set_validates_before_saving() {
        let env = Env::new();
        env.add_profile("home", &[]);
        env.gitaway()
            .args(["profile", "set", "home", "transport.wireguard.mtu", "1400"])
            .assert()
            .success();
        env.gitaway()
            .args(["profile", "set", "home", "hosts.\"192.0.2.10\"", "10.0.0.1"])
            .assert()
            .success();
        let file = env.path("config/gitaway/profiles/home.toml");
        let before = std::fs::read_to_string(&file).unwrap();
        assert!(before.contains("mtu = 1400") && before.contains("\"192.0.2.10\" = \"10.0.0.1\""));

        env.gitaway()
            .args([
                "profile",
                "set",
                "home",
                "transport.wireguard.endpoint",
                "no-port.example.net",
            ])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("needs a port"));
        env.gitaway()
            .args(["profile", "set", "home", "private_key", "x"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("unknown field"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    }

    #[test]
    fn remove_needs_yes_outside_a_terminal() {
        let env = Env::new();
        env.add_profile("home", &[]);
        env.gitaway()
            .args(["profile", "remove", "home"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("--yes"));
        assert!(env.path("config/gitaway/profiles/home.toml").exists());
    }

    #[test]
    fn use_pins_the_repository() {
        let env = Env::new();
        env.add_profile("home", &[]);
        let repo = env.repo("app", "https://example.com/app.git");
        env.gitaway()
            .args(["-C", "app", "use", "home"])
            .assert()
            .success();
        assert_eq!(env.git(&repo, &["config", "gitaway.profile"]), "home");

        env.gitaway()
            .args(["-C", "app", "use", "missing"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("doesn't exist"));
    }
}

/// The Ed25519 public key for an all-zero seed: a standard test vector.
const HOST_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDtqJ7zOtqQtYqOo0CpvDXNlMhV3HeJDpjrASKGLWdop";

#[cfg(unix)]
mod network {
    use super::*;
    use russh::keys::PrivateKey;
    use russh::keys::ssh_key::private::Ed25519Keypair;
    use std::sync::Arc;

    struct Server;
    impl russh::server::Handler for Server {
        type Error = russh::Error;
    }

    /// Starts an SSH server on 127.0.0.1 with a host key from `seed`.
    /// Returns its port and public key line.
    fn ssh_server(seed: u8) -> (u16, String) {
        let key = PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]));
        let public = key.public_key().to_openssh().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap().port()).unwrap();
                let config = Arc::new(russh::server::Config {
                    keys: vec![key],
                    ..Default::default()
                });
                while let Ok((socket, _)) = listener.accept().await {
                    let config = Arc::clone(&config);
                    tokio::spawn(async move {
                        if let Ok(session) = russh::server::run_stream(config, socket, Server).await
                        {
                            let _ = session.await;
                        }
                    });
                }
            });
        });
        (rx.recv().unwrap(), public)
    }

    /// The remotes in these tests point at 127.0.0.1 port 1. git never
    /// really connects (`GIT_SSH_COMMAND=false`), so when gitaway lets git
    /// run, git fails with 128; when gitaway refuses, it exits with 1.
    const REMOTE: &str = "ssh://git@127.0.0.1:1/alice/app.git";

    fn home_profile(env: &Env, name: &str, ssh_port: u16, host_key: &str) {
        env.add_profile(
            name,
            &[
                "--home-ssh",
                &format!("127.0.0.1:{ssh_port}"),
                "--host-key",
                host_key,
                "--host",
                "127.0.0.1=10.0.0.1",
            ],
        );
    }

    fn ran(env: &Env, needle: &str) -> bool {
        env.git_log().iter().any(|line| line.contains(needle))
    }

    #[test]
    fn at_home_git_runs() {
        let env = Env::new();
        let (port, key) = ssh_server(1);
        home_profile(&env, "home", port, &key);
        env.repo("app", REMOTE);

        env.gitaway()
            .args(["-C", "app", "ls-remote"])
            .assert()
            .code(128);
        assert!(ran(&env, "-C app ls-remote"), "{:?}", env.git_log());
    }

    #[test]
    fn a_different_host_key_refuses_and_says_so() {
        let env = Env::new();
        let (port, _) = ssh_server(1);
        home_profile(&env, "home", port, HOST_KEY);
        env.repo("app", REMOTE);

        env.gitaway()
            .args(["-C", "app", "fetch"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("different host key"));
        assert!(!ran(&env, "-C app fetch"), "{:?}", env.git_log());
    }

    #[test]
    fn away_refuses_with_the_reason() {
        let env = Env::new();
        // Documentation address: nothing answers.
        env.add_profile(
            "home",
            &[
                "--home-ssh",
                "192.0.2.1:22",
                "--host-key",
                HOST_KEY,
                "--host",
                "127.0.0.1=10.0.0.1",
            ],
        );
        env.repo("app", REMOTE);

        env.gitaway()
            .args(["-C", "app", "push"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains(
                "not on the home network for profile `home`",
            ));
        assert!(!ran(&env, "-C app push"));
    }

    #[test]
    fn remotes_the_profile_does_not_handle_skip_the_check() {
        let env = Env::new();
        env.add_profile(
            "home",
            &[
                "--home-ssh",
                "192.0.2.1:22",
                "--host-key",
                HOST_KEY,
                "--host",
                "192.0.2.10=10.0.0.1",
                "--default",
            ],
        );
        env.repo("app", "ssh://git@203.0.113.5/alice/app.git");

        // The default profile is away, but it doesn't handle 203.0.113.5.
        env.gitaway()
            .args(["-C", "app", "fetch"])
            .assert()
            .code(128);
        assert!(ran(&env, "-C app fetch"));
    }

    #[test]
    fn clone_at_home_pins_the_profile() {
        let env = Env::new();
        let (port, key) = ssh_server(2);
        home_profile(&env, "home", port, &key);

        env.gitaway()
            .args(["clone", REMOTE, "dest"])
            .assert()
            .code(128);
        let expected = format!("clone --config gitaway.profile=home {REMOTE} dest");
        assert!(env.git_log().contains(&expected), "{:?}", env.git_log());
    }

    #[test]
    fn a_named_remote_is_used() {
        let env = Env::new();
        env.add_profile(
            "home",
            &[
                "--home-ssh",
                "192.0.2.1:22",
                "--host-key",
                HOST_KEY,
                "--host",
                "127.0.0.1=10.0.0.1",
            ],
        );
        let repo = env.repo("app", REMOTE);
        env.git(
            &repo,
            &["remote", "add", "mirror", "https://example.com/app.git"],
        );

        // origin would need the (failing) home check; mirror isn't handled.
        env.gitaway()
            .args(["-C", "app", "fetch", "mirror"])
            .assert()
            .code(128);
        env.gitaway()
            .args(["-C", "app", "fetch", "origin"])
            .assert()
            .code(1);
    }

    #[test]
    fn ambiguous_profiles_are_an_error() {
        let env = Env::new();
        for name in ["home", "cabin"] {
            env.add_profile(
                name,
                &[
                    "--home-ssh",
                    "192.0.2.1:22",
                    "--host-key",
                    HOST_KEY,
                    "--host",
                    "127.0.0.1=10.0.0.1",
                ],
            );
        }
        env.repo("app", REMOTE);
        env.gitaway()
            .args(["-C", "app", "fetch"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("gitaway use"));
    }

    #[test]
    fn profile_flag_and_env_are_checked() {
        let env = Env::new();
        env.add_profile("home", &[]);
        env.repo("app", REMOTE);
        env.gitaway()
            .args(["--profile", "missing", "-C", "app", "fetch"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("profile `missing`"));
        env.gitaway()
            .args(["-C", "app", "fetch"])
            .env("GITAWAY_PROFILE", "Bad Name")
            .assert()
            .code(1)
            .stderr(predicate::str::contains("GITAWAY_PROFILE"));
    }

    #[test]
    fn git_reports_its_own_errors_with_its_own_code() {
        let env = Env::new();
        env.add_profile("home", &["--host", "127.0.0.1=10.0.0.1"]);
        env.gitaway()
            .args(["-C", "no-such-dir", "fetch"])
            .assert()
            .code(128)
            .stderr(
                predicate::str::contains("cannot change to")
                    .and(predicate::str::contains("gitaway").not()),
            );
    }

    #[test]
    fn a_profile_without_home_refuses_handled_remotes() {
        let env = Env::new();
        env.add_profile("home", &["--host", "127.0.0.1=10.0.0.1"]);
        env.repo("app", REMOTE);
        env.gitaway()
            .args(["-C", "app", "fetch"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("no [home] section"));
    }
}
