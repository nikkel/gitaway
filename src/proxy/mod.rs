//! The per-command proxy git talks to when you're away from home.
//!
//! git's URLs stay exactly as they are. For one command, gitaway runs a
//! SOCKS5 server on `127.0.0.1` and points git at it:
//!
//! ```text
//!  git ──▶ https:// via http.proxy=socks5h://…        ┐
//!      ──▶ ssh://   via ssh -o ProxyCommand=helper    ┘─▶ Proxy (127.0.0.1:<random>)
//!                                                              │
//!                                                              ▼
//!                  host:port in the profile's [hosts]? ── yes ─▶ Transport::connect(tunnel address)
//!                                                     └── no ──▶ a direct connection, as git would make
//!                                                                (or none, for a "home only" request)
//! ```
//!
//! Because git still sees the real host name, TLS certificates, credential
//! helpers and `known_hosts` all keep working. The tunnel is only used for
//! the profile's `[hosts]`; anything else git contacts during the command
//! (a submodule on a public forge, say) is connected directly.
//!
//! Only clients that know the random per-command token can use the proxy,
//! and it only listens on loopback. It stops when the [`Proxy`] is dropped.
//!
//! A running `gitaway tunnel` starts a second kind with
//! [`Proxy::start_shared`]: one for other gitaway processes, which borrow
//! its tunnel through it (see [`crate::transport::shared`]). That kind only
//! connects addresses inside the tunnel that its own `[hosts]` lists, and
//! never connects anything directly.
//!
//! - [`socks`]: the SOCKS5 protocol.
//! - [`helper`]: `gitaway __proxy-connect`, the small program ssh runs to
//!   reach the proxy.
//! - [`git_env`]: the environment that points git at the proxy.

pub mod git_env;
pub mod helper;
pub mod socks;

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};
use zeroize::Zeroizing;

use crate::config::profile::HostMap;
use crate::transport::{Transport, TransportError};
use socks::{Reply, Scope, Target};

/// The environment variable telling the helper where the proxy is.
pub const PROXY_ENV: &str = "GITAWAY_PROXY";
/// The environment variable holding the proxy's token for the helper.
pub const TOKEN_ENV: &str = "GITAWAY_PROXY_TOKEN";

/// How long a client gets to finish the SOCKS handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a direct connection may take.
const DIRECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A running proxy. Dropping it closes the listener and every connection.
pub struct Proxy {
    address: SocketAddr,
    token: Zeroizing<String>,
    failures: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

/// Who a proxy is for, which decides the requests it accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Serves {
    /// git and ssh during one command: [`Scope::Any`] and
    /// [`Scope::HomeOnly`] requests.
    Git,
    /// Other gitaway processes borrowing a running `gitaway tunnel`:
    /// [`Scope::TunnelAddress`] requests only.
    OtherCommands,
}

impl Proxy {
    /// Starts a proxy for git on `127.0.0.1` with a random port and token.
    /// Hosts in `hosts` go through `transport`.
    pub async fn start(transport: Arc<dyn Transport>, hosts: HostMap) -> io::Result<Self> {
        Self::start_serving(Serves::Git, transport, hosts).await
    }

    /// Starts a proxy for other gitaway processes on `127.0.0.1` with a
    /// random port and token. It only accepts [`Scope::TunnelAddress`]
    /// requests, for addresses on the tunnel side of `hosts`, and connects
    /// them through `transport`.
    pub async fn start_shared(transport: Arc<dyn Transport>, hosts: HostMap) -> io::Result<Self> {
        Self::start_serving(Serves::OtherCommands, transport, hosts).await
    }

    async fn start_serving(
        serves: Serves,
        transport: Arc<dyn Transport>,
        hosts: HostMap,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let token = new_token()?;
        let failures = Arc::new(Mutex::new(Vec::new()));
        let task = tokio::spawn(serve(
            listener,
            Arc::new(Routes {
                serves,
                transport,
                hosts,
            }),
            token.clone(),
            failures.clone(),
        ));
        Ok(Self {
            address,
            token,
            failures,
            task,
        })
    }

    /// Where the proxy listens.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The token clients must present.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Why connections through the tunnel failed, in order, so gitaway can
    /// explain a failure that git only reports as a proxy error. Always
    /// empty for a [`Proxy::start_shared`] proxy, which may run for days.
    pub fn failures(&self) -> Vec<String> {
        self.failures
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        // Aborting the accept loop drops its JoinSet, which aborts every
        // connection task.
        self.task.abort();
    }
}

struct Routes {
    serves: Serves,
    transport: Arc<dyn Transport>,
    hosts: HostMap,
}

/// 32 random bytes, hex-encoded.
fn new_token() -> io::Result<Zeroizing<String>> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    getrandom::fill(bytes.as_mut()).map_err(|e| io::Error::other(e.to_string()))?;
    let mut token = Zeroizing::new(String::with_capacity(64));
    for byte in bytes.iter() {
        token.push_str(&format!("{byte:02x}"));
    }
    Ok(token)
}

async fn serve(
    listener: TcpListener,
    routes: Arc<Routes>,
    token: Zeroizing<String>,
    failures: Arc<Mutex<Vec<String>>>,
) {
    let token = Arc::new(token);
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((client, _)) = accepted else { continue };
                connections.spawn(handle(client, routes.clone(), token.clone(), failures.clone()));
            }
            // Reap finished connections so the set doesn't grow.
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

/// One client: handshake, connect where the routes say, then relay bytes.
async fn handle(
    mut client: TcpStream,
    routes: Arc<Routes>,
    token: Arc<Zeroizing<String>>,
    failures: Arc<Mutex<Vec<String>>>,
) {
    let target =
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, socks::accept(&mut client, &token)).await {
            Ok(Ok(target)) => target,
            Ok(Err(_)) | Err(_) => return,
        };
    let mut upstream = match open(&routes, &target).await {
        Ok(upstream) => upstream,
        Err((reply, reason)) => {
            if let Some(reason) = reason
                && routes.serves == Serves::Git
            {
                failures
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(reason);
            }
            let _ = socks::reply(&mut client, reply).await;
            return;
        }
    };
    if socks::reply(&mut client, Reply::Succeeded).await.is_err() {
        return;
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    let _ = upstream.shutdown().await;
}

/// Connects to the target: through the tunnel if the profile handles it,
/// otherwise directly, unless the client asked for a home host only. On
/// failure, the SOCKS reply and, for the tunnel, a reason to show the user.
///
/// A home host whose tunnel connection fails is never retried directly.
async fn open(
    routes: &Routes,
    target: &Target,
) -> Result<Box<dyn crate::transport::Stream>, (Reply, Option<String>)> {
    match (routes.serves, target.scope) {
        (Serves::OtherCommands, Scope::TunnelAddress) => {
            return tunnel_address(routes, target).await;
        }
        (Serves::OtherCommands, _) | (Serves::Git, Scope::TunnelAddress) => {
            return Err((Reply::NotAllowed, None));
        }
        (Serves::Git, Scope::Any | Scope::HomeOnly) => {}
    }
    if let Some(tunnel_address) = routes.hosts.route(&target.host, target.port) {
        return through_tunnel(routes, tunnel_address, target).await;
    }
    if target.scope == Scope::HomeOnly {
        return Err((Reply::NotAllowed, None));
    }
    let direct = TcpStream::connect((target.host.as_str(), target.port));
    match tokio::time::timeout(DIRECT_TIMEOUT, direct).await {
        Ok(Ok(stream)) => Ok(Box::new(stream)),
        Ok(Err(e)) if e.kind() == io::ErrorKind::ConnectionRefused => {
            Err((Reply::ConnectionRefused, None))
        }
        Ok(Err(_)) => Err((Reply::HostUnreachable, None)),
        Err(_) => Err((Reply::TimedOut, None)),
    }
}

/// Connects a [`Scope::TunnelAddress`] request: an IP address on the tunnel
/// side of `[hosts]`, through the tunnel. Anything else is not allowed.
async fn tunnel_address(
    routes: &Routes,
    target: &Target,
) -> Result<Box<dyn crate::transport::Stream>, (Reply, Option<String>)> {
    let address = target
        .host
        .parse()
        .map(|ip| SocketAddr::new(ip, target.port))
        .ok()
        .filter(|address| routes.hosts.reaches(*address))
        .ok_or((Reply::NotAllowed, None))?;
    through_tunnel(routes, address, target).await
}

/// Connects to `tunnel_address` through the tunnel, for a request for
/// `target`.
async fn through_tunnel(
    routes: &Routes,
    tunnel_address: SocketAddr,
    target: &Target,
) -> Result<Box<dyn crate::transport::Stream>, (Reply, Option<String>)> {
    routes.transport.connect(tunnel_address).await.map_err(|e| {
        let reply = match &e {
            TransportError::Timeout { .. } | TransportError::NoHandshake { .. } => Reply::TimedOut,
            TransportError::Connect { source, .. }
                if source.kind() == io::ErrorKind::ConnectionRefused =>
            {
                Reply::ConnectionRefused
            }
            _ => Reply::GeneralFailure,
        };
        (reply, Some(format!("{}:{}: {e}", target.host, target.port)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::ConnectFuture;
    use std::io::{Read, Write};

    /// A transport that "tunnels" to 127.0.0.1 on the target's port, and
    /// records where it was asked to go.
    struct Loopback {
        asked: Mutex<Vec<SocketAddr>>,
    }

    impl Transport for Loopback {
        fn connect(&self, target: SocketAddr) -> ConnectFuture<'_> {
            self.asked.lock().unwrap().push(target);
            Box::pin(async move {
                let local = SocketAddr::from((Ipv4Addr::LOCALHOST, target.port()));
                let stream = TcpStream::connect(local)
                    .await
                    .map_err(|source| TransportError::Connect { target, source })?;
                Ok(Box::new(stream) as Box<dyn crate::transport::Stream>)
            })
        }
    }

    fn hosts(text: &str) -> HostMap {
        let profile = crate::config::profile::Profile::from_toml(&format!(
            "[transport.wireguard]\nendpoint = \"vpn.example.net:51820\"\n\
             server_public_key = \"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"\n[hosts]\n{text}"
        ))
        .unwrap();
        profile.hosts
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    /// A one-shot server on 127.0.0.1 that sends `greeting`. Returns its port.
    async fn greeter(greeting: &'static [u8]) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream.write_all(greeting).await.unwrap();
        });
        port
    }

    fn read_greeting(proxy: SocketAddr, token: &str, host: &str, port: u16) -> io::Result<String> {
        let mut stream = socks::connect(proxy, token, host, port)?;
        let mut text = String::new();
        stream.read_to_string(&mut text)?;
        Ok(text)
    }

    #[test]
    fn hosts_in_the_profile_go_through_the_tunnel_and_others_go_direct() {
        runtime().block_on(async {
            let tunnel_port = greeter(b"via tunnel").await;
            let direct_port = greeter(b"direct").await;
            let transport = Arc::new(Loopback {
                asked: Mutex::new(Vec::new()),
            });
            let proxy = Proxy::start(
                transport.clone(),
                hosts("\"git.example.com\" = \"10.0.0.1\""),
            )
            .await
            .unwrap();
            let (address, token) = (proxy.address(), proxy.token().to_owned());

            let tunnelled = tokio::task::spawn_blocking({
                let token = token.clone();
                move || read_greeting(address, &token, "git.example.com", tunnel_port)
            });
            assert_eq!(tunnelled.await.unwrap().unwrap(), "via tunnel");
            assert_eq!(
                transport.asked.lock().unwrap().as_slice(),
                [SocketAddr::from(([10, 0, 0, 1], tunnel_port))]
            );

            let direct = tokio::task::spawn_blocking(move || {
                read_greeting(address, &token, "127.0.0.1", direct_port)
            });
            assert_eq!(direct.await.unwrap().unwrap(), "direct");
            assert_eq!(
                transport.asked.lock().unwrap().len(),
                1,
                "direct didn't use the tunnel"
            );
        });
    }

    #[test]
    fn a_wrong_token_gets_nothing() {
        runtime().block_on(async {
            let transport = Arc::new(Loopback {
                asked: Mutex::new(Vec::new()),
            });
            let proxy = Proxy::start(transport.clone(), hosts("\"192.0.2.10\" = \"10.0.0.1\""))
                .await
                .unwrap();
            let address = proxy.address();
            let result = tokio::task::spawn_blocking(move || {
                socks::connect(address, "guess", "192.0.2.10", 22).map(|_| ())
            })
            .await
            .unwrap();
            assert!(result.is_err());
            assert!(transport.asked.lock().unwrap().is_empty());
        });
    }

    #[test]
    fn tunnel_failures_are_kept_for_the_user() {
        runtime().block_on(async {
            // Nothing listens on the port the tunnel maps to.
            let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = closed.local_addr().unwrap().port();
            drop(closed);
            let transport = Arc::new(Loopback {
                asked: Mutex::new(Vec::new()),
            });
            let proxy = Proxy::start(transport, hosts("\"192.0.2.10\" = \"10.0.0.1\""))
                .await
                .unwrap();
            let (address, token) = (proxy.address(), proxy.token().to_owned());
            let result = tokio::task::spawn_blocking(move || {
                socks::connect(address, &token, "192.0.2.10", port).map(|_| ())
            })
            .await
            .unwrap();
            assert!(result.is_err());
            let failures = proxy.failures();
            assert_eq!(failures.len(), 1, "{failures:?}");
            assert!(
                failures[0].starts_with(&format!("192.0.2.10:{port}")),
                "{failures:?}"
            );
        });
    }

    /// A tunnel that never connects.
    struct Broken;

    impl Transport for Broken {
        fn connect(&self, target: SocketAddr) -> ConnectFuture<'_> {
            Box::pin(async move { Err(TransportError::Timeout { target }) })
        }
    }

    /// The `[hosts]` host is reachable directly here (it's 127.0.0.1), so a
    /// fallback to a direct connection would succeed. It must not happen:
    /// away from home, that address may be someone else's machine.
    #[test]
    fn a_failed_tunnel_never_falls_back_to_a_direct_connection() {
        runtime().block_on(async {
            let direct = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            direct.set_nonblocking(true).unwrap();
            let port = direct.local_addr().unwrap().port();
            let proxy = Proxy::start(Arc::new(Broken), hosts("\"127.0.0.1\" = \"10.0.0.1\""))
                .await
                .unwrap();
            let (address, token) = (proxy.address(), proxy.token().to_owned());
            let result = tokio::task::spawn_blocking(move || {
                socks::connect(address, &token, "127.0.0.1", port).map(|_| ())
            })
            .await
            .unwrap();
            assert!(result.is_err(), "the proxy reported success");
            // The proxy answers only after its connection attempt, so a
            // direct connection would already be waiting here.
            let accepted = direct.accept();
            assert!(
                matches!(&accepted, Err(e) if e.kind() == io::ErrorKind::WouldBlock),
                "the proxy connected directly: {accepted:?}"
            );
            assert_eq!(proxy.failures().len(), 1);
        });
    }

    /// A home-only request for a host outside `[hosts]` connects nowhere,
    /// even though it's reachable directly; one for a home host uses the
    /// tunnel.
    #[test]
    fn home_only_requests_never_connect_directly() {
        runtime().block_on(async {
            let direct = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            direct.set_nonblocking(true).unwrap();
            let port = direct.local_addr().unwrap().port();
            let tunnel_port = greeter(b"via tunnel").await;
            let transport = Arc::new(Loopback {
                asked: Mutex::new(Vec::new()),
            });
            let proxy = Proxy::start(transport.clone(), hosts("\"nas\" = \"10.0.0.1\""))
                .await
                .unwrap();
            let (address, token) = (proxy.address(), proxy.token().to_owned());

            let outside = tokio::task::spawn_blocking({
                let token = token.clone();
                move || socks::connect_home_only(address, &token, "127.0.0.1", port)
            });
            assert!(outside.await.unwrap().unwrap().is_none());
            let accepted = direct.accept();
            assert!(
                matches!(&accepted, Err(e) if e.kind() == io::ErrorKind::WouldBlock),
                "the proxy connected directly: {accepted:?}"
            );

            let home = tokio::task::spawn_blocking(move || {
                let mut stream = socks::connect_home_only(address, &token, "nas", tunnel_port)?
                    .expect("nas is a home host");
                let mut text = String::new();
                stream.read_to_string(&mut text).map(|_| text)
            });
            assert_eq!(home.await.unwrap().unwrap(), "via tunnel");
            assert_eq!(
                transport.asked.lock().unwrap().as_slice(),
                [SocketAddr::from(([10, 0, 0, 1], tunnel_port))]
            );
        });
    }

    /// Asks the proxy at `proxy` for `target`, inside the tunnel. Returns
    /// the greeting it reads, or the proxy's reply code.
    fn read_tunnel_address(proxy: SocketAddr, token: &str, target: &str) -> Result<String, u8> {
        let target = target.parse().unwrap();
        let mut stream =
            socks::connect_tunnel_address(proxy, token, target, Duration::from_secs(5)).unwrap()?;
        let mut text = String::new();
        stream.read_to_string(&mut text).unwrap();
        Ok(text)
    }

    #[test]
    fn the_shared_proxy_connects_only_addresses_inside_the_tunnel_from_its_hosts() {
        runtime().block_on(async {
            let tunnel_port = greeter(b"via tunnel").await;
            let direct = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            direct.set_nonblocking(true).unwrap();
            let direct_port = direct.local_addr().unwrap().port();
            let transport = Arc::new(Loopback {
                asked: Mutex::new(Vec::new()),
            });
            let proxy = Proxy::start_shared(
                transport.clone(),
                hosts("\"192.0.2.10\" = \"10.0.0.1\"\n\"127.0.0.1\" = \"10.0.0.2\""),
            )
            .await
            .unwrap();
            let (address, token) = (proxy.address(), proxy.token().to_owned());

            let listed = tokio::task::spawn_blocking({
                let token = token.clone();
                move || read_tunnel_address(address, &token, &format!("10.0.0.1:{tunnel_port}"))
            });
            assert_eq!(listed.await.unwrap(), Ok("via tunnel".to_owned()));

            // Not on the tunnel side of [hosts]: refused, the tunnel unused.
            let unlisted = tokio::task::spawn_blocking({
                let token = token.clone();
                move || read_tunnel_address(address, &token, "10.0.0.9:22")
            });
            assert_eq!(unlisted.await.unwrap(), Err(Reply::NotAllowed as u8));
            assert_eq!(transport.asked.lock().unwrap().len(), 1);

            // An ordinary request, for a host it could reach directly, or a
            // home host by name: neither is something it serves.
            for host in ["127.0.0.1", "192.0.2.10"] {
                let token = token.clone();
                let ordinary = tokio::task::spawn_blocking(move || {
                    socks::connect(address, &token, host, direct_port).map(|_| ())
                });
                assert!(ordinary.await.unwrap().is_err(), "{host}");
            }
            let accepted = direct.accept();
            assert!(
                matches!(&accepted, Err(e) if e.kind() == io::ErrorKind::WouldBlock),
                "the shared proxy connected directly: {accepted:?}"
            );
            assert_eq!(transport.asked.lock().unwrap().len(), 1);
            assert!(proxy.failures().is_empty());
        });
    }

    #[test]
    fn the_proxy_for_git_refuses_requests_for_addresses_inside_the_tunnel() {
        runtime().block_on(async {
            let transport = Arc::new(Loopback {
                asked: Mutex::new(Vec::new()),
            });
            let proxy = Proxy::start(transport.clone(), hosts("\"192.0.2.10\" = \"10.0.0.1\""))
                .await
                .unwrap();
            let (address, token) = (proxy.address(), proxy.token().to_owned());
            let result = tokio::task::spawn_blocking(move || {
                read_tunnel_address(address, &token, "10.0.0.1:22")
            });
            assert_eq!(result.await.unwrap(), Err(Reply::NotAllowed as u8));
            assert!(transport.asked.lock().unwrap().is_empty());
        });
    }

    #[test]
    fn tokens_are_long_and_random() {
        let a = new_token().unwrap();
        let b = new_token().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(*a, *b);
    }

    #[test]
    fn dropping_the_proxy_stops_it() {
        runtime().block_on(async {
            let transport = Arc::new(Loopback {
                asked: Mutex::new(Vec::new()),
            });
            let proxy = Proxy::start(transport, HostMap::default()).await.unwrap();
            let address = proxy.address();
            drop(proxy);
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            let mut stream = std::net::TcpStream::connect(address);
            if let Ok(stream) = stream.as_mut() {
                // If the port is still accepting, nothing may answer.
                stream.write_all(&[5, 1, 2]).ok();
                stream
                    .set_read_timeout(Some(Duration::from_millis(200)))
                    .unwrap();
                let mut answer = [0; 2];
                assert!(stream.read_exact(&mut answer).is_err());
            }
        });
    }
}
