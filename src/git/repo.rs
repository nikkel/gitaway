//! What gitaway reads from, and writes to, a repository's git config.
//!
//! Everything goes through the `git` binary (via [`Git`]), never by parsing
//! `.git/config` directly. That way `includeIf`, global and system config,
//! `-c` overrides and `url.<base>.insteadOf` rewrites all behave exactly as
//! git itself sees them.
//!
//! # Extending
//!
//! A new query is a method on [`Repo`] that runs one git command through
//! [`Git::capture`] and maps git's exit codes explicitly. Don't treat "any
//! failure" as "not set": that hides real errors.

use super::classify::Direction;
use super::runner::{Git, RunError};

/// The git config key that pins a repository to a profile.
pub const PROFILE_KEY: &str = "gitaway.profile";

/// Queries a repository (or, outside one, the user's global git config).
#[derive(Debug, Clone)]
pub struct Repo {
    git: Git,
}

impl Repo {
    /// A repository as seen by `git` with the user's global options.
    pub fn new(git: Git) -> Self {
        Self { git }
    }

    /// `git config gitaway.profile`: repo, then global, then `includeIf`, as
    /// git resolves it. `None` if it isn't set.
    pub fn profile(&self) -> Result<Option<String>, RunError> {
        self.config_get(PROFILE_KEY)
    }

    /// Pins the current repository to a profile (`git config --local`). Fails
    /// outside a repository.
    pub fn set_profile(&self, name: &str) -> Result<(), RunError> {
        let args = ["config", "--local", PROFILE_KEY, name];
        let out = self.git.capture(&args)?;
        match out.code {
            0 => Ok(()),
            code => Err(failed(&args, code, out.stderr)),
        }
    }

    /// The names of the configured remotes, such as `origin`.
    pub fn remote_names(&self) -> Result<Vec<String>, RunError> {
        let out = self.git.capture(&["remote"])?;
        match out.code {
            0 => Ok(out.stdout.lines().map(str::to_owned).collect()),
            // Outside a repository there are no remotes.
            128 => Ok(Vec::new()),
            code => Err(failed(&["remote"], code, out.stderr)),
        }
    }

    /// Every URL git may connect to for a remote, after any `insteadOf`
    /// rewrites. For [`Direction::Push`] these are the push URLs, which may
    /// differ: git pushes to each one. Empty if there's no such remote.
    ///
    /// A fetch only uses the first URL, but all are listed: counting one
    /// git won't contact only costs a tunnel it didn't need.
    pub fn remote_urls(&self, remote: &str, direction: Direction) -> Result<Vec<String>, RunError> {
        let mut args = vec!["remote", "get-url", "--all"];
        if direction == Direction::Push {
            args.push("--push");
        }
        args.extend(["--", remote]);
        let out = self.git.capture(&args)?;
        match out.code {
            0 => Ok(out.stdout.lines().map(str::to_owned).collect()),
            // 2: no such remote. 128: not in a repository.
            2 | 128 => Ok(Vec::new()),
            code => Err(failed(&args, code, out.stderr)),
        }
    }

    /// The URLs of the repository's submodules: those already set up
    /// (`submodule.<name>.url` in its config) and those listed in its
    /// `.gitmodules`, which `git submodule update --init` reads. Empty
    /// outside a repository or in a bare one.
    ///
    /// Relative URLs (`../lib.git`) are returned as written. They're on the
    /// superproject's own server, which its remotes already cover.
    pub fn submodule_urls(&self) -> Result<Vec<String>, RunError> {
        const KEYS: &str = r"^submodule\..*\.url$";
        let mut urls = self.config_values(&["config", "--get-regexp", KEYS])?;
        let args = ["rev-parse", "--show-toplevel"];
        let top = self.git.capture(&args)?;
        match top.code {
            0 => {
                let file = format!("{}/.gitmodules", top.stdout);
                urls.extend(self.config_values(&[
                    "config",
                    "--file",
                    &file,
                    "--get-regexp",
                    KEYS,
                ])?);
            }
            // Not in a repository, or no working tree.
            128 => {}
            code => return Err(failed(&args, code, top.stderr)),
        }
        Ok(urls)
    }

    /// The values `git config --get-regexp` prints, one per `key value`
    /// line. Exit code 1 means none matched (or the file doesn't exist).
    fn config_values(&self, args: &[&str]) -> Result<Vec<String>, RunError> {
        let out = self.git.capture(args)?;
        match out.code {
            0 => Ok(out
                .stdout
                .lines()
                .filter_map(|line| line.split_once(' ').map(|(_, value)| value.to_owned()))
                .collect()),
            1 => Ok(Vec::new()),
            code => Err(failed(args, code, out.stderr)),
        }
    }

    /// A URL as git will really use it, after `url.<base>.insteadOf`
    /// rewrites. Doesn't touch the network.
    pub fn expand_url(&self, url: &str) -> Result<String, RunError> {
        let args = ["ls-remote", "--get-url", "--", url];
        let out = self.git.capture(&args)?;
        match out.code {
            0 => Ok(out.stdout),
            code => Err(failed(&args, code, out.stderr)),
        }
    }

    /// The remote a bare `git fetch` or `git push` would use, following git's
    /// own rules:
    ///
    /// - push: `branch.<b>.pushRemote`, then `remote.pushDefault`, then
    ///   `branch.<b>.remote`, then `origin`;
    /// - fetch: `branch.<b>.remote`, then `origin`.
    ///
    /// On a detached HEAD there's no branch, so only `remote.pushDefault` (for
    /// push) and `origin` apply.
    pub fn default_remote(&self, direction: Direction) -> Result<String, RunError> {
        let branch = self.current_branch()?;
        let branch_key = |suffix: &str| branch.as_ref().map(|b| format!("branch.{b}.{suffix}"));

        let mut keys = Vec::new();
        if direction == Direction::Push {
            keys.extend(branch_key("pushRemote"));
            keys.push("remote.pushDefault".to_owned());
        }
        keys.extend(branch_key("remote"));

        for key in keys {
            if let Some(remote) = self.config_get(&key)? {
                return Ok(remote);
            }
        }
        Ok("origin".to_owned())
    }

    /// The checked-out branch, or `None` on a detached HEAD or outside a
    /// repository.
    fn current_branch(&self) -> Result<Option<String>, RunError> {
        let args = ["symbolic-ref", "--quiet", "--short", "HEAD"];
        let out = self.git.capture(&args)?;
        match out.code {
            0 => Ok(Some(out.stdout)),
            // 1: detached HEAD. 128: not in a repository.
            1 | 128 => Ok(None),
            code => Err(failed(&args, code, out.stderr)),
        }
    }

    /// The proxy git would use for `url`: `http.proxy` or the most specific
    /// `http.<url>.proxy`, from every config source, as git resolves it.
    pub fn http_proxy_for(&self, url: &str) -> Result<Option<String>, RunError> {
        let args = ["config", "--get-urlmatch", "http.proxy", url];
        let out = self.git.capture(&args)?;
        match out.code {
            0 => Ok(Some(out.stdout)),
            1 => Ok(None),
            code => Err(failed(&args, code, out.stderr)),
        }
    }

    /// `remote.<name>.proxy`, which git ranks above `http.proxy` for that
    /// remote.
    pub fn remote_proxy(&self, remote: &str) -> Result<Option<String>, RunError> {
        self.config_get(&format!("remote.{remote}.proxy"))
    }

    /// `git config --get <key>`. Exit code 1 means the key isn't set.
    /// `core.sshCommand`, if set, including any `-c core.sshCommand=…`
    /// typed before the command.
    pub fn ssh_command(&self) -> Result<Option<String>, RunError> {
        self.config_get("core.sshCommand")
    }

    fn config_get(&self, key: &str) -> Result<Option<String>, RunError> {
        let args = ["config", "--get", key];
        let out = self.git.capture(&args)?;
        match out.code {
            0 => Ok(Some(out.stdout)),
            1 => Ok(None),
            code => Err(failed(&args, code, out.stderr)),
        }
    }
}

fn failed(args: &[&str], code: i32, stderr: String) -> RunError {
    RunError::Failed {
        command: args.join(" "),
        code,
        stderr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    /// git isolated from the user's own config, run in `dir`.
    fn git_in(dir: &Path) -> Git {
        Git::new(&["-C".into(), dir.into()])
            .env("GIT_CONFIG_GLOBAL", null_device())
            .env("GIT_CONFIG_NOSYSTEM", "1")
    }

    fn null_device() -> &'static str {
        if cfg!(windows) { "NUL" } else { "/dev/null" }
    }

    /// A new repository on branch `main`.
    fn repo() -> (TempDir, Repo, Git) {
        let dir = TempDir::new().unwrap();
        let git = git_in(dir.path());
        run(&git, &["init", "--quiet", "--initial-branch=main"]);
        (dir, Repo::new(git.clone()), git)
    }

    fn run(git: &Git, args: &[&str]) {
        let out = git.capture(args).unwrap();
        assert_eq!(out.code, 0, "git {args:?}: {}", out.stderr);
    }

    #[test]
    fn profile_round_trip() {
        let (_dir, repo, _git) = repo();
        assert_eq!(repo.profile().unwrap(), None);
        repo.set_profile("home").unwrap();
        assert_eq!(repo.profile().unwrap().as_deref(), Some("home"));
    }

    #[test]
    fn set_profile_outside_a_repository_fails() {
        let dir = TempDir::new().unwrap();
        let repo = Repo::new(git_in(dir.path()));
        assert!(matches!(
            repo.set_profile("home"),
            Err(RunError::Failed { .. })
        ));
    }

    #[test]
    fn outside_a_repository_there_is_nothing() {
        let dir = TempDir::new().unwrap();
        let repo = Repo::new(git_in(dir.path()));
        assert_eq!(repo.profile().unwrap(), None);
        assert!(repo.remote_names().unwrap().is_empty());
        assert!(
            repo.remote_urls("origin", Direction::Fetch)
                .unwrap()
                .is_empty()
        );
        assert!(repo.submodule_urls().unwrap().is_empty());
        assert_eq!(repo.default_remote(Direction::Push).unwrap(), "origin");
    }

    #[test]
    fn remote_urls_follow_push_urls_and_insteadof() {
        let (_dir, repo, git) = repo();
        run(&git, &["remote", "add", "origin", "lan:alice/app.git"]);
        run(
            &git,
            &["config", "url.ssh://git@192.0.2.10:2222/.insteadOf", "lan:"],
        );
        run(
            &git,
            &["remote", "add", "mirror", "https://example.com/app.git"],
        );
        run(
            &git,
            &[
                "remote",
                "set-url",
                "--push",
                "mirror",
                "ssh://git@example.net/app.git",
            ],
        );

        assert_eq!(repo.remote_names().unwrap(), ["mirror", "origin"]);
        assert_eq!(
            repo.remote_urls("origin", Direction::Fetch).unwrap(),
            ["ssh://git@192.0.2.10:2222/alice/app.git"]
        );
        assert_eq!(
            repo.remote_urls("mirror", Direction::Fetch).unwrap(),
            ["https://example.com/app.git"]
        );
        assert_eq!(
            repo.remote_urls("mirror", Direction::Push).unwrap(),
            ["ssh://git@example.net/app.git"]
        );
        assert!(
            repo.remote_urls("missing", Direction::Fetch)
                .unwrap()
                .is_empty()
        );

        // git pushes to every push URL, so every one is listed.
        run(
            &git,
            &[
                "remote",
                "set-url",
                "--add",
                "--push",
                "mirror",
                "ssh://git@192.0.2.10/app.git",
            ],
        );
        assert_eq!(
            repo.remote_urls("mirror", Direction::Push).unwrap(),
            [
                "ssh://git@example.net/app.git",
                "ssh://git@192.0.2.10/app.git"
            ]
        );
    }

    #[test]
    fn submodule_urls_come_from_gitmodules_and_the_config() {
        let (dir, repo, git) = repo();
        std::fs::write(
            dir.path().join(".gitmodules"),
            "[submodule \"lib\"]\n\tpath = lib\n\turl = ssh://git@192.0.2.10/lib.git\n",
        )
        .unwrap();
        run(
            &git,
            &[
                "config",
                "submodule.docs.url",
                "https://example.com/docs.git",
            ],
        );
        assert_eq!(
            repo.submodule_urls().unwrap(),
            [
                "https://example.com/docs.git",
                "ssh://git@192.0.2.10/lib.git"
            ]
        );
    }

    #[test]
    fn urls_are_expanded_with_insteadof() {
        let (_dir, repo, git) = repo();
        run(
            &git,
            &["config", "url.ssh://git@192.0.2.10:2222/.insteadOf", "lan:"],
        );
        assert_eq!(
            repo.expand_url("lan:alice/app.git").unwrap(),
            "ssh://git@192.0.2.10:2222/alice/app.git"
        );
        assert_eq!(
            repo.expand_url("https://example.com/app.git").unwrap(),
            "https://example.com/app.git"
        );
    }

    #[test]
    fn remote_names_starting_with_a_dash_are_not_options() {
        let (_dir, repo, _git) = repo();
        assert!(
            repo.remote_urls("--push", Direction::Fetch)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn default_remote_follows_gits_rules() {
        let (_dir, repo, git) = repo();
        assert_eq!(repo.default_remote(Direction::Fetch).unwrap(), "origin");

        run(&git, &["config", "branch.main.remote", "upstream"]);
        assert_eq!(repo.default_remote(Direction::Fetch).unwrap(), "upstream");
        assert_eq!(repo.default_remote(Direction::Push).unwrap(), "upstream");

        run(&git, &["config", "remote.pushDefault", "fork"]);
        assert_eq!(repo.default_remote(Direction::Fetch).unwrap(), "upstream");
        assert_eq!(repo.default_remote(Direction::Push).unwrap(), "fork");

        run(&git, &["config", "branch.main.pushRemote", "mine"]);
        assert_eq!(repo.default_remote(Direction::Push).unwrap(), "mine");
    }

    #[test]
    fn detached_head_falls_back() {
        let (_dir, repo, git) = repo();
        run(
            &git,
            &[
                "-c",
                "user.name=alice",
                "-c",
                "user.email=alice@example.com",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "x",
            ],
        );
        run(&git, &["checkout", "--quiet", "--detach"]);
        run(&git, &["config", "branch.main.remote", "upstream"]);
        assert_eq!(repo.default_remote(Direction::Fetch).unwrap(), "origin");
    }
}
