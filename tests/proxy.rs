//! Real git and ssh through gitaway's proxy, with the real helper binary.
//!
//! The "tunnel" here is a stand-in transport that connects to 127.0.0.1, so
//! these tests cover the git side: that each kind of URL really goes
//! through the proxy, to the address in `[hosts]`, without its URL being
//! changed. The home addresses are documentation addresses (192.0.2.x) that
//! can't be reached directly, so a test only passes if the proxy carried it.

mod common;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

use gitaway::config::profile::{HostMap, Profile};
use gitaway::proxy::Proxy;
use gitaway::proxy::git_env::{self, SshSettings};
use gitaway::transport::{ConnectFuture, Stream, Transport, TransportError};
use tempfile::TempDir;

/// Connects "through the tunnel" to 127.0.0.1 on the target's port, and
/// records each target.
#[derive(Default)]
struct Loopback {
    asked: Mutex<Vec<SocketAddr>>,
}

impl Transport for Loopback {
    fn connect(&self, target: SocketAddr) -> ConnectFuture<'_> {
        self.asked.lock().unwrap().push(target);
        Box::pin(async move {
            let local = SocketAddr::from((Ipv4Addr::LOCALHOST, target.port()));
            let stream = tokio::net::TcpStream::connect(local)
                .await
                .map_err(|source| TransportError::Connect { target, source })?;
            Ok(Box::new(stream) as Box<dyn Stream>)
        })
    }
}

fn home_hosts() -> HostMap {
    Profile::from_toml(
        r#"
        [transport.wireguard]
        endpoint = "vpn.example.net:51820"
        server_public_key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
        [hosts]
        "192.0.2.10" = "10.0.0.1"
        "#,
    )
    .unwrap()
    .hosts
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// An isolated git environment in a temporary directory.
struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let sandbox = Self {
            dir: TempDir::new().unwrap(),
        };
        std::fs::write(sandbox.path("gitconfig"), "").unwrap();
        sandbox
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    /// git, isolated from the user's configuration.
    fn git(&self) -> Command {
        let mut git = Command::new("git");
        git.current_dir(self.dir.path())
            .env("HOME", self.dir.path())
            .env("GIT_CONFIG_GLOBAL", self.path("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_SSH_COMMAND")
            .env_remove("GIT_SSH")
            .env_remove("GIT_CONFIG_COUNT");
        git
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Runs git (blocking) with the proxy's environment, while the proxy runs
/// on the runtime.
async fn git_through_proxy(git: Command, proxy: &Proxy, ssh: SshSettings<'static>) -> Output {
    let env = git_env::build(
        proxy.address(),
        proxy.token(),
        Path::new(env!("CARGO_BIN_EXE_gitaway")),
        ssh,
        None,
    )
    .unwrap();
    tokio::task::spawn_blocking(move || {
        let mut git = git;
        for (name, value) in &env.set {
            git.env(name, value);
        }
        for name in &env.remove {
            git.env_remove(name);
        }
        git.output().unwrap()
    })
    .await
    .unwrap()
}

#[test]
fn ssh_reaches_the_server_through_the_tunnel_despite_a_proxyjump() {
    let sandbox = Sandbox::new();
    let (port, _) = common::ssh_server(7);
    // A ProxyJump in the user's ssh config must not route around the tunnel.
    std::fs::write(
        sandbox.path("ssh_config"),
        "Host 192.0.2.10\n  ProxyJump nowhere.example.net\n",
    )
    .unwrap();
    let ssh_command: &'static str = Box::leak(
        format!(
            "ssh -F '{}' -o BatchMode=yes -o StrictHostKeyChecking=no \
             -o UserKnownHostsFile=/dev/null -o IdentitiesOnly=yes -o IdentityFile=/dev/null",
            sandbox.path("ssh_config").display()
        )
        .into_boxed_str(),
    );
    runtime().block_on(async {
        let transport = Arc::new(Loopback::default());
        let proxy = Proxy::start(transport.clone(), home_hosts()).await.unwrap();
        let mut git = sandbox.git();
        git.arg("ls-remote")
            .arg(format!("ssh://git@192.0.2.10:{port}/app.git"));
        let ssh = SshSettings {
            git_ssh_command: Some(ssh_command),
            ..Default::default()
        };
        let out = git_through_proxy(git, &proxy, ssh).await;
        // The test server accepts no logins: being refused one proves ssh
        // got there, which it could only do through the proxy.
        assert!(!out.status.success());
        assert!(
            stderr(&out).contains("Permission denied"),
            "{}",
            stderr(&out)
        );
        assert_eq!(
            transport.asked.lock().unwrap().as_slice(),
            [SocketAddr::from(([10, 0, 0, 1], port))]
        );
    });
}

#[test]
fn http_goes_through_the_tunnel_and_the_proxy_password_is_never_stored() {
    let sandbox = Sandbox::new();
    // A credential helper that records every call.
    let log = sandbox.path("credential.log");
    std::fs::write(
        sandbox.path("gitconfig"),
        format!(
            "[credential]\n\thelper = \"!f() {{ echo \\\"$1\\\" >> '{}'; cat >> '{}'; }}; f\"\n",
            log.display(),
            log.display()
        ),
    )
    .unwrap();
    runtime().block_on(async {
        let port = common::empty_http_server().await;
        let transport = Arc::new(Loopback::default());
        let proxy = Proxy::start(transport.clone(), home_hosts()).await.unwrap();
        let mut git = sandbox.git();
        git.arg("ls-remote")
            .arg(format!("http://192.0.2.10:{port}/app.git"));
        let out = git_through_proxy(git, &proxy, SshSettings::default()).await;
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(
            transport
                .asked
                .lock()
                .unwrap()
                .contains(&SocketAddr::from(([10, 0, 0, 1], port)))
        );
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            !calls.contains("socks5h"),
            "the proxy password reached the credential helper:\n{calls}"
        );
        assert!(!calls.contains(proxy.token()), "{calls}");
    });
}

/// Shows the protection above is needed: without the scoped empty helper,
/// git hands the proxy password to the credential helper to store. If git
/// ever stops doing that, this test fails and the protection can go.
#[test]
fn without_the_scoped_helper_git_would_store_the_proxy_password() {
    let sandbox = Sandbox::new();
    let log = sandbox.path("credential.log");
    std::fs::write(
        sandbox.path("gitconfig"),
        format!(
            "[credential]\n\thelper = \"!f() {{ echo \\\"$1\\\" >> '{}'; cat >> '{}'; }}; f\"\n",
            log.display(),
            log.display()
        ),
    )
    .unwrap();
    runtime().block_on(async {
        let port = common::empty_http_server().await;
        let transport = Arc::new(Loopback::default());
        let proxy = Proxy::start(transport, home_hosts()).await.unwrap();
        let mut git = sandbox.git();
        git.arg("ls-remote")
            .arg(format!("http://192.0.2.10:{port}/app.git"))
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.proxy")
            .env(
                "GIT_CONFIG_VALUE_0",
                format!("socks5h://gitaway:{}@{}", proxy.token(), proxy.address()),
            );
        let out = tokio::task::spawn_blocking(move || git.output().unwrap())
            .await
            .unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
        let calls = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            calls.contains("store") && calls.contains("socks5h"),
            "git no longer stores proxy passwords:\n{calls}"
        );
    });
}
