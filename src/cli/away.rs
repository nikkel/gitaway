//! Running a network command away from home, through the profile's tunnel.
//!
//! ```text
//!  git:// remote? ──────────────────────────────▶ error: use ssh:// or https://
//!  ssh home URL, and your ssh command sets its own
//!  ProxyCommand/ProxyJump? ──────────────────────▶ error: move it to ~/.ssh/config
//!  https home URL, and a one-off proxy (-c …)
//!  would outrank gitaway's? ─────────────────────▶ error: run it without that
//!  `gitaway tunnel` running for the profile? ─▶ borrow its tunnel
//!    otherwise: take the profile's lock (wait if it's busy), load the device
//!    key, open the tunnel
//!  start the proxy ─▶ run git as a child, pointed at the proxy
//!  wait for git (Ctrl+C reaches git too; a SIGTERM to gitaway is passed on)
//!  git failed and the tunnel knows why? ────────▶ say why
//!  close the proxy and the tunnel, exit with git's code
//! ```
//!
//! Unlike at home, gitaway can't hand over to git with `exec`: it must stay
//! alive to run the tunnel, so it spawns git and waits.

use std::ffi::OsString;
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use crate::error::Error;
use crate::git::remote_url::Scheme;
use crate::git::repo::Repo;
use crate::git::runner::{Git, exit_code, spawn_error};
use crate::proxy::Proxy;
use crate::proxy::git_env::{self, HomeHttp, SshSettings};
use crate::transport::Transport;

use super::network::{AwayPlan, Contact};
use super::tunnel_access;

/// How long to wait for background tasks to stop after git has finished.
pub(crate) const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

/// Runs git through the tunnel and returns git's exit code.
pub fn run(plan: AwayPlan) -> Result<i32, Error> {
    if plan.home.iter().any(|home| home.url.scheme == Scheme::Git) {
        return Err(Error::msg(format!(
            "git:// remotes can't go through the tunnel (profile `{}`). Use the server's ssh:// \
             or https:// URL instead: `git remote set-url <remote> <url>`",
            plan.name
        )));
    }

    let repo = Repo::new(Git::new(&plan.global_options));
    let core_ssh_command = repo.ssh_command()?;
    let git_ssh_command = std::env::var("GIT_SSH_COMMAND").ok();
    let git_ssh = std::env::var("GIT_SSH").ok();
    let ssh = SshSettings {
        git_ssh_command: git_ssh_command.as_deref(),
        core_ssh_command: core_ssh_command.as_deref(),
        git_ssh: git_ssh.as_deref(),
    };
    if let Some(home) = plan.home.iter().find(|home| home.url.scheme == Scheme::Ssh) {
        git_env::check_ssh(ssh, &home.url.host).map_err(Error::msg)?;
    }
    let (http_urls, http_remotes) = home_http(&plan.home);
    let exe = std::env::current_exe()
        .map_err(|e| Error::msg(format!("couldn't find gitaway's own path: {e}")))?;
    let home_http = HomeHttp {
        urls: &http_urls,
        remotes: &http_remotes,
    };
    check_one_off_proxy(&plan, &exe, ssh, home_http)?;

    let store = super::open_store()?;
    let name = &plan.name;
    let (_lock, source) = tunnel_access::for_command(&store, name)?;

    let runtime = runtime()?;
    let result = runtime.block_on(async {
        let transport: Arc<dyn Transport> = Arc::from(source.open(name, &plan.profile).await?);
        let proxy = Proxy::start(transport, plan.profile.hosts.clone())
            .await
            .map_err(|e| Error::msg(format!("couldn't start the local proxy: {e}")))?;

        let existing_count = std::env::var("GIT_CONFIG_COUNT").ok();
        let env = git_env::build(
            proxy.address(),
            proxy.token(),
            &exe,
            ssh,
            home_http,
            existing_count.as_deref(),
        )
        .map_err(Error::msg)?;

        let status = run_git(&plan.args, &env).await?;
        let code = exit_code(status);
        if code != 0 {
            for failure in proxy.failures() {
                eprintln!("gitaway: {failure}");
            }
        }
        Ok(code)
    });
    // Everything spawned on the runtime (proxy, tunnel) stops here.
    runtime.shutdown_timeout(SHUTDOWN_GRACE);
    result
}

/// Refuses a proxy that git would rank above gitaway's for a home URL: one
/// given for this command alone (`-c`, `--config-env`, or
/// `GIT_CONFIG_PARAMETERS`), which outranks the settings gitaway passes in
/// git's environment. Settings in config files don't; gitaway outranks
/// those.
///
/// Rather than guess from option names, it asks git which proxy it would use,
/// with a stand-in for gitaway's: the same environment, minus the real
/// address and token. That runs before any tunnel is opened.
fn check_one_off_proxy(
    plan: &AwayPlan,
    exe: &std::path::Path,
    ssh: SshSettings<'_>,
    home: HomeHttp<'_>,
) -> Result<(), Error> {
    if home.urls.is_empty() {
        return Ok(());
    }
    let stand_in = std::net::SocketAddr::from(([127, 0, 0, 1], 9));
    let token = "stand-in";
    let existing_count = std::env::var("GIT_CONFIG_COUNT").ok();
    let env = git_env::build(stand_in, token, exe, ssh, home, existing_count.as_deref())
        .map_err(Error::msg)?;
    let git = env
        .set
        .iter()
        .fold(Git::new(&plan.global_options), |git, (name, value)| {
            git.env(name, value)
        });
    let repo = Repo::new(git);
    let expected = git_env::proxy_url(stand_in, token);

    let outranked = |host: &str| {
        Error::msg(format!(
            "a proxy set for this command alone (with -c, --config-env or \
             GIT_CONFIG_PARAMETERS) outranks gitaway's own, so it would take {host} around \
             gitaway's tunnel. Run the command without it. To use that proxy for other \
             servers, put it in your git config instead (`git config --global \
             http.<url>.proxy …`): gitaway outranks settings there for home servers"
        ))
    };
    for contact in plan
        .home
        .iter()
        .filter(|home| matches!(home.url.scheme, Scheme::Http | Scheme::Https))
    {
        if repo.http_proxy_for(&contact.text)?.as_deref() != Some(expected.as_str()) {
            return Err(outranked(&contact.url.host));
        }
        if let Some(remote) = &contact.remote
            && repo.remote_proxy(remote)?.as_deref() != Some(expected.as_str())
        {
            return Err(outranked(&contact.url.host));
        }
    }
    Ok(())
}

/// The home URLs reached over HTTP, and the remotes they belong to, whose
/// proxy settings gitaway outranks for the command.
fn home_http(home: &[Contact]) -> (Vec<&str>, Vec<&str>) {
    let over_http: Vec<&Contact> = home
        .iter()
        .filter(|home| matches!(home.url.scheme, Scheme::Http | Scheme::Https))
        .collect();
    let urls = over_http.iter().map(|home| home.text.as_str()).collect();
    let mut remotes: Vec<&str> = over_http
        .iter()
        .filter_map(|home| home.remote.as_deref())
        .collect();
    remotes.sort_unstable();
    remotes.dedup();
    (urls, remotes)
}

/// Starts the small runtime the tunnel and proxy run on.
pub(crate) fn runtime() -> Result<tokio::runtime::Runtime, Error> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| Error::msg(format!("couldn't start the tunnel's runtime: {e}")))
}

/// Spawns git with the proxy's environment and waits for it.
async fn run_git(args: &[OsString], env: &git_env::GitEnv) -> Result<ExitStatus, Error> {
    let mut command = Git::new(&[]).command(args);
    for (name, value) in &env.set {
        command.env(name, value);
    }
    for name in &env.remove {
        command.env_remove(name);
    }
    let mut child = tokio::process::Command::from(command)
        .spawn()
        .map_err(spawn_error)?;
    wait(&mut child)
        .await
        .map_err(|e| Error::msg(format!("lost track of git: {e}")))
}

/// Waits for git. Ctrl+C, Ctrl+\ and a closed terminal reach git directly
/// (it's in the same process group), so gitaway just keeps waiting for it
/// to finish. A SIGTERM sent to gitaway alone is passed on to git.
#[cfg(unix)]
async fn wait(child: &mut tokio::process::Child) -> std::io::Result<ExitStatus> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut quit = signal(SignalKind::quit())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut terminate = signal(SignalKind::terminate())?;
    loop {
        tokio::select! {
            status = child.wait() => return status,
            _ = interrupt.recv() => {}
            _ = quit.recv() => {}
            _ = hangup.recv() => {}
            _ = terminate.recv() => forward_terminate(child),
        }
    }
}

#[cfg(unix)]
fn forward_terminate(child: &tokio::process::Child) {
    use rustix::process::{Pid, Signal, kill_process};
    let pid = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(Pid::from_raw);
    if let Some(pid) = pid {
        // If git has already exited, there's nothing to tell.
        let _ = kill_process(pid, Signal::TERM);
    }
}

/// Waits for git. Ctrl+C reaches git directly, so gitaway keeps waiting.
#[cfg(not(unix))]
async fn wait(child: &mut tokio::process::Child) -> std::io::Result<ExitStatus> {
    loop {
        tokio::select! {
            status = child.wait() => return status,
            _ = tokio::signal::ctrl_c() => {}
        }
    }
}
