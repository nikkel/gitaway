//! End-to-end tests: run the real `gitaway` binary.
//!
//! Each test gets its own temporary home and config directory, with the
//! user's git config and real SSH kept out (`GIT_CONFIG_GLOBAL`,
//! `GIT_SSH_COMMAND=false`). On Unix, `git` on `PATH` is a wrapper that logs
//! each command line and then runs the real git, so tests can check exactly
//! which git commands gitaway ran, or that it ran none.
//!
//! Device keys go to files in the temporary config directory
//! (`GITAWAY_KEY_STORE=file`), never the real keychain.

mod common;

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
        Command::from_std(self.gitaway_std())
    }

    /// The same, as a plain `std` command, for tests that need its process.
    fn gitaway_std(&self) -> StdCommand {
        let mut cmd = StdCommand::new(env!("CARGO_BIN_EXE_gitaway"));
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
            .env("GITAWAY_KEY_STORE", "file")
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
    #[cfg(unix)]
    fn git_log(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("git.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Whether gitaway ran a git command line containing `needle`. Only
    /// Unix has the logging git, so only Unix tests can ask.
    #[cfg(unix)]
    fn ran(&self, needle: &str) -> bool {
        self.git_log().iter().any(|line| line.contains(needle))
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

    /// The device key file for a profile.
    fn key_file(&self, name: &str) -> PathBuf {
        self.path(&format!("config/gitaway/keys/{name}.key"))
    }

    /// Creates a profile whose WireGuard server has the public key `server`.
    fn add_profile_with_server(&self, name: &str, server: &str, extra: &[&str]) {
        self.gitaway()
            .args([
                "profile",
                "add",
                name,
                "--endpoint",
                "127.0.0.1:9",
                "--server-public-key",
                server,
            ])
            .args(extra)
            .assert()
            .success();
    }

    /// The device's WireGuard public key for a profile, from `gitaway peer`.
    fn device_public_key(&self, name: &str) -> String {
        let out = self.gitaway().args(["peer", name]).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("PublicKey = "))
            .expect("peer prints a PublicKey")
            .to_owned()
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
        for command in ["list", "tunnel"] {
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

    use common::ssh_server;

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
        assert!(env.ran("-C app ls-remote"), "{:?}", env.git_log());
    }

    #[test]
    fn a_different_host_key_means_away_and_says_so() {
        let env = Env::new();
        let (port, _) = ssh_server(1);
        home_profile(&env, "home", port, HOST_KEY);
        env.repo("app", REMOTE);

        // Away, so it tries the tunnel: this profile has no tunnel address.
        env.gitaway()
            .args(["-C", "app", "fetch"])
            .assert()
            .code(1)
            .stderr(
                predicate::str::contains("different host key")
                    .and(predicate::str::contains("has no tunnel address")),
            );
        assert!(!env.ran("-C app fetch"), "{:?}", env.git_log());
    }

    #[test]
    fn away_without_a_tunnel_address_says_how_to_add_one() {
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
                "gitaway profile set home transport.wireguard.address",
            ));
        assert!(!env.ran("-C app push"));
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
        assert!(env.ran("-C app fetch"));
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
    fn a_profile_without_home_always_uses_the_tunnel() {
        let env = Env::new();
        env.add_profile("home", &["--host", "127.0.0.1=10.0.0.1"]);
        env.repo("app", REMOTE);
        env.gitaway()
            .args(["-C", "app", "fetch"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("has no tunnel address"));
        assert!(!env.ran("-C app fetch"));
    }
}

mod keys {
    use super::*;

    /// The public key in `gitaway peer`'s block.
    fn peer_public_key(env: &Env, name: &str) -> String {
        let out = env.gitaway().args(["peer", name]).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).unwrap();
        stdout
            .lines()
            .find_map(|line| line.strip_prefix("PublicKey = "))
            .unwrap_or_else(|| panic!("no PublicKey in {stdout}"))
            .to_owned()
    }

    #[test]
    fn add_creates_a_private_key_that_peer_and_show_agree_on() {
        let env = Env::new();
        env.add_profile("home", &["--address", "10.0.0.2/24"]);
        let key_file = std::fs::read_to_string(env.key_file("home")).unwrap();

        let public = peer_public_key(&env, "home");
        assert_ne!(
            key_file.trim(),
            public,
            "the private key never leaves the file"
        );
        env.gitaway()
            .args(["peer", "home"])
            .assert()
            .success()
            .stdout(
                predicate::str::starts_with("[Peer]")
                    .and(predicate::str::contains("AllowedIPs = 10.0.0.2/32"))
                    .and(predicate::str::contains(key_file.trim()).not()),
            );
        env.gitaway()
            .args(["profile", "show", "home"])
            .assert()
            .success()
            .stdout(
                predicate::str::contains(format!("device public key: {public}"))
                    .and(predicate::str::contains(key_file.trim()).not()),
            );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(env.key_file("home"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn peer_without_an_address_explains_what_to_do() {
        let env = Env::new();
        env.add_profile("home", &[]);
        env.gitaway()
            .args(["peer", "home"])
            .assert()
            .success()
            .stdout(predicate::str::contains("AllowedIPs = <"))
            .stderr(predicate::str::contains(
                "gitaway profile set home transport.wireguard.address",
            ));
    }

    #[test]
    fn peer_uses_the_default_profile_and_needs_one() {
        let env = Env::new();
        env.add_profile("home", &[]);
        env.gitaway()
            .arg("peer")
            .assert()
            .code(1)
            .stderr(predicate::str::contains("no profile chosen"));
        env.gitaway()
            .args(["profile", "default", "home"])
            .assert()
            .success();
        env.gitaway()
            .arg("peer")
            .assert()
            .success()
            .stdout(predicate::str::contains("# gitaway profile `home`"));
    }

    #[test]
    fn peer_never_creates_a_key() {
        let env = Env::new();
        env.add_profile("home", &[]);
        std::fs::remove_file(env.key_file("home")).unwrap();
        env.gitaway()
            .args(["peer", "home"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("gitaway key rotate home"));
        assert!(!env.key_file("home").exists());
    }

    #[test]
    fn rotate_replaces_the_key_only_when_confirmed() {
        let env = Env::new();
        env.add_profile("home", &[]);
        let before = peer_public_key(&env, "home");

        env.gitaway()
            .args(["key", "rotate", "home"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("--yes"));
        assert_eq!(peer_public_key(&env, "home"), before);

        env.gitaway()
            .args(["key", "rotate", "home", "--yes"])
            .assert()
            .success()
            .stdout(predicate::str::contains("[Peer]"));
        assert_ne!(peer_public_key(&env, "home"), before);
    }

    #[test]
    fn rotate_creates_a_missing_key_without_asking() {
        let env = Env::new();
        env.add_profile("home", &[]);
        std::fs::remove_file(env.key_file("home")).unwrap();
        env.gitaway()
            .args(["key", "rotate", "home"])
            .assert()
            .success()
            .stderr(predicate::str::contains("Created the device key"));
        assert!(env.key_file("home").exists());
    }

    /// RFC 7748 section 6.1: Alice's private and public keys.
    const IMPORTED_PRIVATE: &str = "dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo=";
    const IMPORTED_PUBLIC: &str = "hSDwCYkwp1R0i33ctD73Wg2/Og0mOBr066SpjqqbTmo=";

    #[test]
    fn import_from_a_pipe_replaces_the_key_only_with_yes() {
        let env = Env::new();
        env.add_profile("home", &["--address", "10.0.0.2/32"]);
        let before = peer_public_key(&env, "home");

        env.gitaway()
            .args(["key", "import", "home"])
            .write_stdin(IMPORTED_PRIVATE)
            .assert()
            .code(1)
            .stderr(predicate::str::contains("--yes"));
        assert_eq!(peer_public_key(&env, "home"), before);

        env.gitaway()
            .args(["key", "import", "home", "--yes"])
            .write_stdin(format!("{IMPORTED_PRIVATE}\n"))
            .assert()
            .success()
            .stdout(
                predicate::str::contains(format!("PublicKey = {IMPORTED_PUBLIC}"))
                    .and(predicate::str::contains(IMPORTED_PRIVATE).not()),
            )
            .stderr(predicate::str::contains(IMPORTED_PRIVATE).not());
        assert_eq!(peer_public_key(&env, "home"), IMPORTED_PUBLIC);
    }

    #[test]
    fn import_takes_the_private_key_from_a_client_config() {
        let env = Env::new();
        env.add_profile("home", &[]);
        std::fs::remove_file(env.key_file("home")).unwrap();
        let config = format!(
            "[Interface]\nPrivateKey = {IMPORTED_PRIVATE}\nAddress = 10.0.0.2/32\n\n\
             [Peer]\nPublicKey = {SERVER_KEY}\nEndpoint = vpn.example.net:51820\n"
        );
        env.gitaway()
            .args(["key", "import", "home"])
            .write_stdin(config)
            .assert()
            .success();
        assert_eq!(peer_public_key(&env, "home"), IMPORTED_PUBLIC);
    }

    #[test]
    fn import_refuses_the_servers_own_key_and_damaged_keys() {
        let env = Env::new();
        // The profile's server is the one whose private key is pasted.
        env.add_profile_with_server("home", IMPORTED_PUBLIC, &[]);
        let before = peer_public_key(&env, "home");
        env.gitaway()
            .args(["key", "import", "home", "--yes"])
            .write_stdin(IMPORTED_PRIVATE)
            .assert()
            .code(1)
            .stderr(predicate::str::contains("server's own private key"));
        env.gitaway()
            .args(["key", "import", "home", "--yes"])
            .write_stdin(&IMPORTED_PRIVATE[..40])
            .assert()
            .code(1)
            .stderr(
                predicate::str::contains("isn't a WireGuard private key")
                    .and(predicate::str::contains(&IMPORTED_PRIVATE[..20]).not()),
            );
        assert_eq!(peer_public_key(&env, "home"), before);
    }

    #[test]
    fn add_ends_with_the_peer_block_for_the_server() {
        let env = Env::new();
        let out = env
            .gitaway()
            .args([
                "profile",
                "add",
                "home",
                "--endpoint",
                "vpn.example.net:51820",
                "--server-public-key",
                SERVER_KEY,
                "--address",
                "10.0.0.2/32",
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        let stdout = String::from_utf8(out.stdout).unwrap();
        let next = stdout.find("Next:").expect("a next step");
        let block = &stdout[next..];
        assert!(
            block.contains(&format!("PublicKey = {}", peer_public_key(&env, "home"))),
            "{stdout}"
        );
        assert!(block.contains("AllowedIPs = 10.0.0.2/32"), "{stdout}");
        assert!(block.contains("gitaway key import home"), "{stdout}");
    }

    #[test]
    fn remove_deletes_the_key() {
        let env = Env::new();
        env.add_profile("home", &[]);
        env.gitaway()
            .args(["profile", "remove", "home", "--yes"])
            .assert()
            .success()
            .stdout(predicate::str::contains("and its device key"));
        assert!(!env.key_file("home").exists());
    }

    #[test]
    fn add_keeps_a_leftover_key() {
        let env = Env::new();
        env.add_profile("home", &[]);
        let before = peer_public_key(&env, "home");
        std::fs::remove_file(env.path("config/gitaway/profiles/home.toml")).unwrap();
        env.add_profile("home", &[]);
        assert_eq!(peer_public_key(&env, "home"), before);
    }

    #[test]
    fn add_leaves_nothing_behind_if_the_key_cannot_be_saved() {
        let env = Env::new();
        // A file where the keys directory should be makes saving the key fail.
        std::fs::create_dir_all(env.path("config/gitaway")).unwrap();
        std::fs::write(env.path("config/gitaway/keys"), "").unwrap();
        env.gitaway()
            .args([
                "profile",
                "add",
                "home",
                "--endpoint",
                "vpn.example.net:51820",
                "--server-public-key",
                SERVER_KEY,
            ])
            .assert()
            .code(1);
        assert!(!env.path("config/gitaway/profiles/home.toml").exists());
    }

    #[test]
    fn an_unknown_key_store_setting_is_an_error() {
        let env = Env::new();
        env.gitaway()
            .args([
                "profile",
                "add",
                "home",
                "--endpoint",
                "vpn.example.net:51820",
                "--server-public-key",
                SERVER_KEY,
            ])
            .env("GITAWAY_KEY_STORE", "vault")
            .assert()
            .code(1)
            .stderr(predicate::str::contains("GITAWAY_KEY_STORE"));
        assert!(!env.path("config/gitaway/profiles/home.toml").exists());
    }
}

mod away {
    use super::*;
    use common::{DEVICE_TUNNEL_ADDRESS, home_server, ssh_server};
    use gitaway::keys::device_key::DeviceKey;

    /// A port on 127.0.0.1 with nothing listening, for a home check that
    /// fails at once.
    fn closed_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// A profile `home` whose home check fails (so gitaway is away), with a
    /// real WireGuard server forwarding `ports`. 192.0.2.10 (unreachable
    /// otherwise) maps to the server's side of the tunnel.
    fn away_profile(env: &Env, ports: &[u16]) {
        let home_ssh = format!("127.0.0.1:{}", closed_port());
        tunnel_profile(
            env,
            &["--home-ssh", &home_ssh, "--host-key", HOST_KEY],
            ports,
        );
    }

    /// A profile `home` (with `flags` added) whose WireGuard server is real
    /// and forwards `ports`, with 192.0.2.10 mapped to the server's side of
    /// the tunnel.
    fn tunnel_profile(env: &Env, flags: &[&str], ports: &[u16]) {
        let server_key = DeviceKey::generate();
        let server_public = server_key.public_key().to_string();
        let address = format!("{DEVICE_TUNNEL_ADDRESS}/32");
        let mut all_flags = vec!["--address", &address, "--host", "192.0.2.10=10.0.0.1"];
        all_flags.extend_from_slice(flags);
        env.add_profile_with_server("home", &server_public, &all_flags);
        let device = env.device_public_key("home");
        let server = home_server(server_key.into(), &device.parse().unwrap(), ports);
        env.gitaway()
            .args([
                "profile",
                "set",
                "home",
                "transport.wireguard.endpoint",
                &format!("127.0.0.1:{}", server.udp_port),
            ])
            .assert()
            .success();
    }

    fn http_server() -> u16 {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                tx.send(common::empty_http_server().await).unwrap();
                std::future::pending::<()>().await;
            });
        });
        rx.recv().unwrap()
    }

    fn profile_text(env: &Env) -> String {
        std::fs::read_to_string(env.path("config/gitaway/profiles/home.toml")).unwrap()
    }

    /// The base64 part of a public key line, which is what's compared.
    fn key_body(line: &str) -> &str {
        line.split_whitespace().nth(1).unwrap()
    }

    #[test]
    fn check_pins_the_home_key_through_the_tunnel_only_when_told() {
        let env = Env::new();
        let (port, server_host_key) = ssh_server(11);
        let home_ssh = format!("192.0.2.10:{port}");
        tunnel_profile(&env, &["--home-ssh", &home_ssh], &[port]);

        env.gitaway()
            .args(["check", "home"])
            .assert()
            .code(1)
            .stdout(
                predicate::str::contains("Tunnel: works")
                    .and(predicate::str::contains("--yes"))
                    .and(predicate::str::contains("no key pinned")),
            );
        assert!(!profile_text(&env).contains("host_key"));

        env.gitaway()
            .args(["check", "home", "--yes"])
            .assert()
            .success()
            .stdout(predicate::str::contains("Pinned it"));
        assert!(profile_text(&env).contains(key_body(&server_host_key)));

        env.gitaway()
            .args(["check", "home"])
            .assert()
            .success()
            .stdout(
                predicate::str::contains("It matches")
                    .and(predicate::str::contains("Home check: away")),
            );
    }

    #[test]
    fn check_warns_when_the_pinned_key_differs_and_replaces_it_only_when_told() {
        let env = Env::new();
        let (port, server_host_key) = ssh_server(12);
        let home_ssh = format!("192.0.2.10:{port}");
        tunnel_profile(
            &env,
            &["--home-ssh", &home_ssh, "--host-key", HOST_KEY],
            &[port],
        );

        env.gitaway()
            .args(["check", "home"])
            .assert()
            .code(1)
            .stdout(predicate::str::contains("WARNING: it differs"));
        assert!(profile_text(&env).contains(key_body(HOST_KEY)));

        env.gitaway()
            .args(["check", "home", "--yes"])
            .assert()
            .success();
        let text = profile_text(&env);
        assert!(text.contains(key_body(&server_host_key)), "{text}");
        assert!(!text.contains(key_body(HOST_KEY)), "{text}");
    }

    #[test]
    fn check_at_home_verifies_the_key_directly_without_the_tunnel_or_device_key() {
        let env = Env::new();
        let (port, server_host_key) = ssh_server(14);
        let home_ssh = format!("127.0.0.1:{port}");
        // The endpoint can't be reached, so using the tunnel would fail.
        env.add_profile(
            "home",
            &[
                "--home-ssh",
                &home_ssh,
                "--host-key",
                &server_host_key,
                "--host",
                "127.0.0.1=10.0.0.1",
            ],
        );
        std::fs::remove_file(env.key_file("home")).unwrap();
        env.gitaway()
            .args(["check", "home"])
            .assert()
            .success()
            .stdout(
                predicate::str::contains("you're home")
                    .and(predicate::str::contains("Tunnel: not tested at home")),
            );
    }

    #[test]
    fn check_needs_to_know_where_home_is_through_the_tunnel() {
        let env = Env::new();
        // 192.0.2.20 isn't in [hosts], so there's no address to try.
        env.add_profile("home", &["--home-ssh", "192.0.2.20:22"]);
        env.gitaway()
            .args(["check", "home"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("'hosts.\"192.0.2.20\"'"));
    }

    #[test]
    fn without_a_pinned_key_commands_use_the_tunnel_and_say_how_to_pin_it() {
        let env = Env::new();
        let port = http_server();
        let (ssh_port, _) = ssh_server(13);
        let home_ssh = format!("127.0.0.1:{ssh_port}");
        tunnel_profile(&env, &["--home-ssh", &home_ssh], &[port]);
        env.gitaway()
            .args(["ls-remote", &format!("http://192.0.2.10:{port}/app.git")])
            .assert()
            .success()
            .stderr(predicate::str::contains("gitaway check home"));
        #[cfg(unix)]
        assert!(
            env.ran("ls-remote http://192.0.2.10"),
            "{:?}",
            env.git_log()
        );
    }

    #[test]
    fn https_style_remotes_work_through_a_real_wireguard_tunnel() {
        let env = Env::new();
        let port = http_server();
        away_profile(&env, &[port]);
        env.gitaway()
            .args(["ls-remote", &format!("http://192.0.2.10:{port}/app.git")])
            .assert()
            .success();
        #[cfg(unix)]
        assert!(
            env.ran("ls-remote http://192.0.2.10"),
            "{:?}",
            env.git_log()
        );
    }

    #[test]
    fn ssh_goes_through_the_tunnel_and_gits_exit_code_comes_back() {
        let env = Env::new();
        let (port, _) = ssh_server(9);
        away_profile(&env, &[port]);
        env.gitaway()
            .args(["ls-remote", &format!("ssh://git@192.0.2.10:{port}/app.git")])
            .env(
                "GIT_SSH_COMMAND",
                "ssh -F /dev/null -o BatchMode=yes -o StrictHostKeyChecking=no \
                 -o UserKnownHostsFile=/dev/null -o IdentitiesOnly=yes -o IdentityFile=/dev/null",
            )
            .assert()
            // The test server allows no logins; being refused one proves ssh
            // reached it, which it could only do through the tunnel.
            .code(128)
            .stderr(predicate::str::contains("Permission denied"));
    }

    #[test]
    fn a_tunnel_failure_is_explained_after_git_fails() {
        let env = Env::new();
        // The server forwards nothing, so connections through it are refused.
        away_profile(&env, &[]);
        env.gitaway()
            .args(["ls-remote", "http://192.0.2.10:3000/app.git"])
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "gitaway: 192.0.2.10:3000: couldn't connect to 10.0.0.1:3000 through the tunnel",
            ));
    }

    #[test]
    fn git_protocol_remotes_are_refused_with_advice() {
        let env = Env::new();
        away_profile(&env, &[]);
        env.gitaway()
            .args(["ls-remote", "git://192.0.2.10/app.git"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains(
                "git:// remotes can't go through the tunnel",
            ));
        #[cfg(unix)]
        assert!(!env.ran("ls-remote git://"), "{:?}", env.git_log());
    }

    /// A lone SIGINT (the terminal sends it to git as well) doesn't make
    /// gitaway abandon a running git; a SIGTERM to gitaway reaches git, and
    /// gitaway exits with git's code for it.
    #[cfg(unix)]
    #[test]
    fn signals_reach_git_and_gitaway_waits_for_it() {
        use std::time::{Duration, Instant};
        let env = Env::new();
        away_profile(&env, &[]);
        let marker = env.path("ssh-started");
        // An "ssh" that never connects: it records that it started, then waits.
        let slow_ssh = format!("touch '{}'; sleep 30; true", marker.display());
        let mut child = env
            .gitaway_std()
            .args(["ls-remote", "ssh://git@192.0.2.10:2222/app.git"])
            .env("GIT_SSH_COMMAND", &slow_ssh)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id().to_string();
        let signal = |name: &str| {
            let status = StdCommand::new("kill").args([name, &pid]).status().unwrap();
            assert!(status.success());
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        while !marker.exists() {
            assert!(Instant::now() < deadline, "git never started ssh");
            std::thread::sleep(Duration::from_millis(20));
        }

        signal("-INT");
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            child.try_wait().unwrap().is_none(),
            "gitaway left git running alone"
        );

        signal("-TERM");
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "gitaway didn't stop after SIGTERM"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        // git died from the forwarded SIGTERM: 128 + 15.
        assert_eq!(status.code(), Some(143));
    }

    #[test]
    fn a_missing_device_key_says_how_to_make_one() {
        let env = Env::new();
        away_profile(&env, &[]);
        std::fs::remove_file(env.key_file("home")).unwrap();
        env.gitaway()
            .args(["ls-remote", "http://192.0.2.10:3000/app.git"])
            .assert()
            .code(1)
            .stderr(predicate::str::contains("gitaway key rotate home"));
    }
}

mod setup {
    use super::*;
    use common::{DEVICE_TUNNEL_ADDRESS, HOME_TUNNEL_ADDRESS, home_server, ssh_server};
    use gitaway::keys::device_key::DeviceKey;

    /// A WireGuard client config, as a server hands one to a device.
    fn client_config(env: &Env, device: &DeviceKey, server: &str, endpoint: &str) -> PathBuf {
        let path = env.path("laptop.conf");
        let text = format!(
            "[Interface]\nPrivateKey = {}\nAddress = {DEVICE_TUNNEL_ADDRESS}/32\n\n\
             [Peer]\nPublicKey = {server}\nAllowedIPs = {HOME_TUNNEL_ADDRESS}/32\n\
             Endpoint = {endpoint}\nPersistentKeepalive = 25\n",
            device.to_base64().as_str()
        );
        std::fs::write(&path, text).unwrap();
        path
    }

    /// A device key, a real WireGuard server that knows it and forwards
    /// `ports`, and the config the server would give the device.
    fn server_and_config(env: &Env, ports: &[u16]) -> (DeviceKey, PathBuf) {
        let device = DeviceKey::generate();
        let server_key = DeviceKey::generate();
        let server_public = server_key.public_key().to_string();
        let server = home_server(server_key.into(), &device.public_key(), ports);
        let endpoint = format!("127.0.0.1:{}", server.udp_port);
        let config = client_config(env, &device, &server_public, &endpoint);
        (device, config)
    }

    fn profile_text(env: &Env, name: &str) -> String {
        std::fs::read_to_string(env.path(&format!("config/gitaway/profiles/{name}.toml"))).unwrap()
    }

    fn nothing_saved(env: &Env, name: &str) -> bool {
        !env.path(&format!("config/gitaway/profiles/{name}.toml"))
            .exists()
            && !env.key_file(name).exists()
    }

    #[test]
    fn away_it_finds_the_git_server_through_the_tunnel_and_saves_a_working_profile() {
        let env = Env::new();
        let (port, host_key) = ssh_server(21);
        let (device, config) = server_and_config(&env, &[port]);
        // 192.0.2.10 answers nowhere, so this device is "away".
        let url = format!("ssh://git@192.0.2.10:{port}/alice/app.git");

        env.gitaway()
            .args(["setup", "--config", config.to_str().unwrap(), "--url", &url])
            .assert()
            .success()
            .stdout(
                predicate::str::contains("Tunnel: works")
                    .and(predicate::str::contains(format!(
                        "Found your git server at {HOME_TUNNEL_ADDRESS}:{port}"
                    )))
                    .and(predicate::str::contains(
                        "`home` is now the default profile",
                    )),
            );

        let text = profile_text(&env, "home");
        assert!(
            text.contains(host_key.split_whitespace().nth(1).unwrap()),
            "{text}"
        );
        assert!(
            text.contains(&format!("\"192.0.2.10\" = \"{HOME_TUNNEL_ADDRESS}\"")),
            "{text}"
        );
        assert_eq!(
            env.device_public_key("home"),
            device.public_key().to_string(),
            "the config's key wasn't kept"
        );
        // The saved profile works on its own.
        env.gitaway()
            .args(["check", "home"])
            .assert()
            .success()
            .stdout(predicate::str::contains("It matches"));
    }

    #[test]
    fn at_home_with_yes_it_pins_the_key_found_directly_without_the_tunnel() {
        let env = Env::new();
        let (port, host_key) = ssh_server(22);
        let device = DeviceKey::generate();
        // Nothing listens at the endpoint: the tunnel must not be needed.
        let config = client_config(&env, &device, SERVER_KEY, "127.0.0.1:9");
        let url = format!("ssh://git@127.0.0.1:{port}/alice/app.git");

        env.gitaway()
            .args([
                "setup",
                "lan",
                "--yes",
                "--config",
                config.to_str().unwrap(),
                "--url",
                &url,
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("Tunnel: not tested"));
        let text = profile_text(&env, "lan");
        assert!(
            text.contains(host_key.split_whitespace().nth(1).unwrap()),
            "{text}"
        );
        env.gitaway()
            .args(["check", "lan"])
            .assert()
            .success()
            .stdout(predicate::str::contains("you're home"));
    }

    #[test]
    fn a_tunnel_without_the_git_server_saves_nothing_and_says_what_was_tried() {
        let env = Env::new();
        // The server forwards nothing, so the git server's port is refused.
        let (_device, config) = server_and_config(&env, &[]);
        env.gitaway()
            .args([
                "setup",
                "--config",
                config.to_str().unwrap(),
                "--url",
                "http://192.0.2.10:3000/alice/app.git",
            ])
            .assert()
            .code(1)
            .stderr(
                predicate::str::contains("didn't answer inside it")
                    .and(predicate::str::contains(format!(
                        "{HOME_TUNNEL_ADDRESS}:3000"
                    )))
                    .and(predicate::str::contains("--inside")),
            );
        assert!(nothing_saved(&env, "home"));
    }

    #[test]
    fn a_server_that_does_not_know_this_device_saves_nothing() {
        let env = Env::new();
        let (port, _) = ssh_server(23);
        let (_device, mut config) = server_and_config(&env, &[port]);
        // Another device's config for the same server: unknown to it.
        let text = std::fs::read_to_string(&config).unwrap();
        let other = DeviceKey::generate();
        let first_line_end = text.find("\nAddress").unwrap();
        let text = format!(
            "[Interface]\nPrivateKey = {}{}",
            other.to_base64().as_str(),
            &text[first_line_end..]
        );
        config = env.path("other.conf");
        std::fs::write(&config, text).unwrap();

        env.gitaway()
            .args([
                "setup",
                "--config",
                config.to_str().unwrap(),
                "--url",
                &format!("ssh://git@192.0.2.10:{port}/alice/app.git"),
            ])
            .assert()
            .code(1)
            .stderr(
                predicate::str::contains("no answer from your VPN server")
                    .and(predicate::str::contains("has added this device")),
            );
        assert!(nothing_saved(&env, "home"));
    }

    #[test]
    fn a_preshared_key_from_the_config_is_kept_and_used() {
        use gitaway::keys::preshared_key::PresharedKey;
        use gitaway::keys::store::DeviceSecrets;
        // An example preshared key (bytes 1 to 32), not a secret.
        const PRESHARED: &str = "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA=";

        let env = Env::new();
        let (port, _) = ssh_server(24);
        let device = DeviceKey::generate();
        let server_key = DeviceKey::generate();
        let server_public = server_key.public_key().to_string();
        // The server refuses any handshake without the same preshared key.
        let server_secrets = DeviceSecrets {
            key: server_key,
            preshared_key: Some(PresharedKey::from_base64(PRESHARED).unwrap()),
        };
        let server = home_server(server_secrets, &device.public_key(), &[port]);
        let endpoint = format!("127.0.0.1:{}", server.udp_port);
        let config = client_config(&env, &device, &server_public, &endpoint);
        let text = std::fs::read_to_string(&config)
            .unwrap()
            .replace("[Peer]\n", &format!("[Peer]\nPresharedKey = {PRESHARED}\n"));
        std::fs::write(&config, text).unwrap();
        let url = format!("ssh://git@192.0.2.10:{port}/alice/app.git");

        env.gitaway()
            .args(["setup", "--config", config.to_str().unwrap(), "--url", &url])
            .assert()
            .success()
            .stdout(predicate::str::contains("Tunnel: works"));
        let shows_preshared = |env: &Env| {
            env.gitaway()
                .args(["profile", "show", "home"])
                .assert()
                .success()
                .stdout(predicate::str::contains("preshared key: yes"));
        };
        shows_preshared(&env);
        // The stored preshared key is used again later.
        env.gitaway()
            .args(["check", "home"])
            .assert()
            .success()
            .stdout(predicate::str::contains("It matches"));
        // A new device key keeps it: it belongs to the server's entry.
        env.gitaway()
            .args(["key", "rotate", "home", "--yes"])
            .assert()
            .success()
            .stderr(predicate::str::contains("preshared key was kept"));
        shows_preshared(&env);
    }

    #[test]
    fn bad_input_is_refused_before_anything_is_saved() {
        let env = Env::new();
        let not_a_config = env.path("notes.txt");
        std::fs::write(&not_a_config, "remember the milk\n").unwrap();
        let setup = |config: &Path, url: &str| {
            env.gitaway()
                .args(["setup", "--config", config.to_str().unwrap(), "--url", url])
                .assert()
                .code(1)
        };
        setup(&not_a_config, "ssh://git@192.0.2.10/app.git")
            .stderr(predicate::str::contains("isn't a VPN client config"));
        setup(&env.path("missing.conf"), "ssh://git@192.0.2.10/app.git")
            .stderr(predicate::str::contains("couldn't open"));

        let config = client_config(&env, &DeviceKey::generate(), SERVER_KEY, "127.0.0.1:9");
        setup(&config, "/srv/git/app.git").stderr(predicate::str::contains("--url"));
        assert!(nothing_saved(&env, "home"));

        env.add_profile("home", &[]);
        setup(&config, "ssh://git@192.0.2.10/app.git")
            .stderr(predicate::str::contains("already exists"));
    }

    #[test]
    fn config_text_given_as_the_path_is_refused_without_repeating_its_key() {
        let env = Env::new();
        let device = DeviceKey::generate();
        let config = client_config(&env, &device, SERVER_KEY, "127.0.0.1:9");
        let text = std::fs::read_to_string(config).unwrap();
        let private = device.to_base64();
        let out = env
            .gitaway()
            .args([
                "setup",
                "--config",
                &text,
                "--url",
                "ssh://git@192.0.2.10/app.git",
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1));
        let shown = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(shown.contains("not the config itself"), "{shown}");
        assert!(
            !shown.contains(&private[..20]),
            "the private key was shown: {shown}"
        );
        assert!(nothing_saved(&env, "home"));
    }

    /// RFC 7748's published test key, not a secret.
    const PRIVATE: &str = "dwdtCnMYpX08FsFyUbJmRd9ML4frwJkqsXf7pR25LCo="; // gitleaks:allow

    /// A key typed after `--config` by mistake, alone or as a config line,
    /// is refused without being repeated, as it would be if read as a path.
    #[test]
    fn a_key_given_as_the_config_path_is_never_echoed() {
        for value in [PRIVATE.to_owned(), format!("PrivateKey = {PRIVATE}")] {
            let env = Env::new();
            let out = env
                .gitaway()
                .args([
                    "setup",
                    "--config",
                    &value,
                    "--url",
                    "ssh://git@192.0.2.10/app.git",
                ])
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(1));
            let shown = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                !shown.contains(&PRIVATE[..20]),
                "the key was shown: {shown}"
            );
        }
    }
}
