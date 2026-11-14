//! `gitaway tunnel`: serve the git server's web interface on this computer,
//! through the profile's tunnel, until Ctrl+C.
//!
//! ```text
//!  [web] url ─── missing? ──▶ ask once and save it (no terminal: say how to set it)
//!  [hosts] routes the url? ── no ──▶ error: which entry to add
//!  home check passes? ─────── yes ─▶ "you're home: open <url>", exit 0
//!  listen on 127.0.0.1:<port> (and [::1]:<port>) ── taken? ──▶ error: try --port
//!  take the profile's lock, load the device key, open the tunnel (with keepalives)
//!    (another `gitaway tunnel` for the profile already running? ──▶ error)
//!  one connection to the web server ── fails? ──▶ say why (no handshake, …)
//!  lend the tunnel: a proxy for other gitaway commands, and runs/<profile>.toml
//!  serve: each local connection ──▶ a connection through the tunnel, bytes copied both ways
//!  Ctrl+C, a closed terminal or SIGTERM ──▶ remove the runs file, close everything, exit 0
//! ```
//!
//! The local port is only reachable from this computer, and only ever
//! forwards to the one web address. It has no login of its own: any program
//! here can use it while the tunnel runs. The git server still asks for its
//! own login. Web pages can't use it under their own name: see
//! [`super::web_guard`].
//!
//! The web interface still believes its address is the home one, so the
//! clone URLs it shows are the home ones, which is what gitaway commands
//! use anyway. Some servers show a warning about the address on their
//! sign-in page.
//!
//! While the tunnel runs it holds the profile's lock, and lends the tunnel
//! to other gitaway commands for the profile, such as a push, through a
//! second loopback proxy that only gitaway uses (see
//! [`crate::config::run_file`] and [`crate::transport::shared`]).

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use toml_edit::value;

use crate::config::name::ProfileName;
use crate::config::profile::{Profile, Web};
use crate::config::run_file::RunFile;
use crate::config::store::Store;
use crate::config::value::WebUrl;
use crate::error::Error;
use crate::home_check::{self, Verdict};
use crate::proxy::Proxy;
use crate::transport::{self, Lifetime, Transport};

use super::web_guard::{self, Opening};
use super::{away, prompt, tunnel_access};

/// How long to pause after the system refuses a new connection (too many
/// open files, say) before accepting again, so the loop doesn't spin.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

/// Runs `gitaway tunnel` for the profile. `port` is the `--port` value, if
/// given. Returns 0 once stopped with Ctrl+C, or at home, where nothing is
/// needed.
pub fn run(store: &Store, name: &ProfileName, port: Option<&str>) -> Result<i32, Error> {
    let profile = store.load_profile(name)?;
    let port = port
        .map(parse_port)
        .transpose()
        .map_err(|e| Error::msg(format!("--port: {e}")))?;
    let web = match &profile.web {
        Some(web) => web.clone(),
        None => ask_web(store, name, &profile)?,
    };
    let target = route(name, &profile, &web.url)?;
    let local_port = port.unwrap_or_else(|| web.local_port());

    // At home the device key isn't needed, so the keychain isn't touched.
    if let Some(home) = &profile.home
        && home_check::check(home)? == Verdict::Home
    {
        println!(
            "You're home, so the tunnel isn't needed: open {} directly.",
            web.url
        );
        return Ok(0);
    }

    // Before the lock and the keychain, so a busy port is reported at once.
    let listeners = listen(local_port).map_err(|e| listen_error(&e, name, local_port))?;
    if web.url.https {
        eprintln!(
            "gitaway: note: {} uses https. Its certificate names the server, not localhost, so \
             your browser will warn about it.",
            web.url
        );
    }

    let (_lock, secrets) = tunnel_access::for_tunnel(store, name)?;
    let runtime = away::runtime()?;
    let local_url = web.url.on_port("localhost", local_port);
    let result = runtime.block_on(async {
        let transport: Arc<dyn Transport> = Arc::from(
            transport::open(name.as_str(), &profile, secrets, Lifetime::LongRunning).await?,
        );
        // One connection now, so a tunnel that can't work says why before
        // anything claims it's up.
        drop(transport.connect(target).await?);
        let lent = Proxy::start_shared(transport.clone(), profile.hosts.clone())
            .await
            .map_err(|e| Error::msg(format!("couldn't start the proxy for other commands: {e}")))?;
        let run_file = RunFile::new(lent.address(), lent.token())
            .publish(store.dir(), name)
            .map_err(|e| Error::msg(format!("couldn't save where the tunnel is: {e}")))?;
        let forwards = start_forwards(listeners, transport, target)?;
        println!(
            "Tunnel for profile `{name}` is up.\n  {local_url}  ->  your git server at {}\n  \
             gitaway commands for `{name}` use this tunnel while it runs.\nPress Ctrl+C to stop.",
            web.url
        );
        stop_requested()
            .await
            .map_err(|e| Error::msg(format!("couldn't watch for Ctrl+C: {e}")))?;
        // Commands stop looking for the tunnel before it goes.
        drop(run_file);
        drop(lent);
        drop(forwards);
        println!("Tunnel closed.");
        Ok(0)
    });
    // Everything spawned on the runtime (forwards, tunnel) stops here.
    runtime.shutdown_timeout(away::SHUTDOWN_GRACE);
    result
}

/// Asks for the web address and saves it in the profile. Without a
/// terminal, says how to set it instead.
fn ask_web(store: &Store, name: &ProfileName, profile: &Profile) -> Result<Web, Error> {
    let example = format!(
        "http://{}:3000",
        profile
            .home
            .as_ref()
            .map_or("192.0.2.10", |home| home.ssh.host.as_str())
    );
    if !prompt::interactive() {
        return Err(Error::msg(format!(
            "profile `{name}` has no web address for the tunnel. Set your git server's web \
             address, as you'd open it at home: `gitaway profile set {name} web.url {example}` \
             (with its real port)"
        )));
    }
    println!(
        "gitaway tunnel needs your git server's web address on the home network: the one you'd \
         open in a browser at home."
    );
    let url = prompt::required(
        None,
        "web.url",
        &format!("Web address (such as {example})"),
        |text| {
            let url: WebUrl = text.parse().map_err(|e| format!("{e}"))?;
            route(name, profile, &url).map_err(|e| e.to_string())?;
            Ok(url)
        },
    )?;
    save_web_url(store, name, &url)?;
    println!("Saved it in profile `{name}`.");
    Ok(Web {
        url,
        local_port: None,
    })
}

/// Saves `url` as the profile's `[web] url`, in a `[web]` section like the
/// rest of the file. (Assigning `doc["web"]["url"]` alone would make an
/// inline `web = { … }` at the top.)
fn save_web_url(store: &Store, name: &ProfileName, url: &WebUrl) -> Result<(), Error> {
    store.update_profile(name, |doc| {
        if !doc.contains_key("web") {
            doc["web"] = toml_edit::table();
        }
        doc["web"]["url"] = value(url.to_string());
        Ok(())
    })?;
    Ok(())
}

/// Where the web address is through the tunnel, from the profile's
/// `[hosts]`.
fn route(name: &ProfileName, profile: &Profile, url: &WebUrl) -> Result<SocketAddr, Error> {
    profile
        .hosts
        .route(&url.address.host, url.port())
        .ok_or_else(|| {
            Error::msg(format!(
                "profile `{name}`'s [hosts] doesn't say where {} is through the tunnel. Add it \
                 with `gitaway profile set {name} 'hosts.\"{}\"' <address through the tunnel>`",
                url.address, url.address.host
            ))
        })
}

fn parse_port(text: &str) -> Result<u16, String> {
    match text.parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ => Err(format!("`{text}` isn't a port number from 1 to 65535")),
    }
}

/// Listens on `port` on this computer's loopback addresses: 127.0.0.1, and
/// [::1] if it has IPv6.
///
/// [::1] counts: browsers often try it first for `localhost`, so if another
/// program already listens there, the browser would reach that program
/// instead. That's an error, like a taken 127.0.0.1.
pub(crate) fn listen(port: u16) -> io::Result<Vec<TcpListener>> {
    let mut listeners = vec![TcpListener::bind((Ipv4Addr::LOCALHOST, port))?];
    match TcpListener::bind((Ipv6Addr::LOCALHOST, port)) {
        Ok(listener) => listeners.push(listener),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::AddrInUse | io::ErrorKind::PermissionDenied
            ) =>
        {
            return Err(e);
        }
        // No IPv6 loopback on this computer.
        Err(_) => {}
    }
    for listener in &listeners {
        listener.set_nonblocking(true)?;
    }
    Ok(listeners)
}

fn listen_error(error: &io::Error, name: &ProfileName, port: u16) -> Error {
    let another_port = format!(
        "Choose another with `gitaway tunnel {name} --port <port>`, or for good with \
         `gitaway profile set {name} web.local_port <port>`"
    );
    Error::msg(match error.kind() {
        io::ErrorKind::AddrInUse => format!(
            "port {port} on this computer is already in use, probably by another program. \
             {another_port}"
        ),
        io::ErrorKind::PermissionDenied => format!(
            "this system only lets administrators use port {port}. {another_port} (above 1023)"
        ),
        _ => format!("couldn't listen on port {port} on this computer: {error}"),
    })
}

/// Starts forwarding each listener's connections to `target` through the
/// tunnel. Dropping the returned set stops them.
fn start_forwards(
    listeners: Vec<TcpListener>,
    transport: Arc<dyn Transport>,
    target: SocketAddr,
) -> Result<tokio::task::JoinSet<()>, Error> {
    let mut forwards = tokio::task::JoinSet::new();
    for listener in listeners {
        let listener = tokio::net::TcpListener::from_std(listener)
            .map_err(|e| Error::msg(format!("couldn't listen on this computer: {e}")))?;
        forwards.spawn(forward(listener, transport.clone(), target));
    }
    Ok(forwards)
}

/// Accepts connections and copies each one to `target` through the tunnel,
/// both ways, until the task is stopped. A connection whose first request
/// names another site is refused before the tunnel is used (see
/// [`web_guard`]).
pub(crate) async fn forward(
    listener: tokio::net::TcpListener,
    transport: Arc<dyn Transport>,
    target: SocketAddr,
) {
    let port = listener.local_addr().map_or(0, |address| address.port());
    let noted_refusal = Arc::new(AtomicBool::new(false));
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let mut local = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(e) => {
                eprintln!("gitaway: couldn't accept a connection: {e}");
                tokio::time::sleep(ACCEPT_RETRY).await;
                continue;
            }
        };
        let transport = transport.clone();
        let noted_refusal = noted_refusal.clone();
        connections.spawn(async move {
            let opening = match web_guard::read_opening(&mut local, port).await {
                Opening::Forward(bytes) => bytes,
                Opening::Refuse => {
                    let _ = local.write_all(web_guard::refusal(port).as_bytes()).await;
                    if !noted_refusal.swap(true, Ordering::Relaxed) {
                        eprintln!(
                            "gitaway: refused a request on localhost:{port} for another site's \
                             name. A web page may have tried to reach your git server through \
                             the tunnel; nothing was sent to it."
                        );
                    }
                    return;
                }
                Opening::Drop => return,
            };
            match transport.connect(target).await {
                Ok(mut remote) => {
                    if remote.write_all(&opening).await.is_err() {
                        return;
                    }
                    // Either side closing or failing ends the connection;
                    // there's nothing more to report.
                    let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
                }
                Err(e) => eprintln!("gitaway: couldn't reach your git server: {e}"),
            }
        });
        // Forget connections that have finished.
        while connections.try_join_next().is_some() {}
    }
}

/// Waits for Ctrl+C, a closed terminal or a request to stop (SIGTERM).
#[cfg(unix)]
async fn stop_requested() -> io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        _ = interrupt.recv() => {}
        _ = hangup.recv() => {}
        _ = terminate.recv() => {}
    }
    Ok(())
}

/// Waits for Ctrl+C or the console window closing.
#[cfg(not(unix))]
async fn stop_requested() -> io::Result<()> {
    let mut close = tokio::signal::windows::ctrl_close()?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result?,
        _ = close.recv() => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{ConnectFuture, Stream, TransportError};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A "tunnel" that connects to 127.0.0.1 instead, standing in for the
    /// home network.
    struct Loopback;

    impl Transport for Loopback {
        fn connect(&self, target: SocketAddr) -> ConnectFuture<'_> {
            Box::pin(async move {
                let local = SocketAddr::from((Ipv4Addr::LOCALHOST, target.port()));
                tokio::net::TcpStream::connect(local)
                    .await
                    .map(|stream| Box::new(stream) as Box<dyn Stream>)
                    .map_err(|source| TransportError::Connect { target, source })
            })
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn the_web_address_is_saved_as_a_web_section() {
        let temp = tempfile::TempDir::new().unwrap();
        let store = Store::new(
            crate::config::paths::ConfigDir::new(Some(temp.path().into()), None).unwrap(),
        );
        let name: ProfileName = "home".parse().unwrap();
        store
            .create_profile(
                &name,
                "[transport.wireguard]\nendpoint = \"vpn.example.net:51820\"\n\
                 server_public_key = \"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"\n\n\
                 [hosts]\n\"192.0.2.10\" = \"10.0.0.1\"\n",
            )
            .unwrap();
        let url: WebUrl = "http://192.0.2.10:3000".parse().unwrap();
        save_web_url(&store, &name, &url).unwrap();

        let text = std::fs::read_to_string(store.dir().profile_file(&name)).unwrap();
        assert!(
            text.ends_with("[web]\nurl = \"http://192.0.2.10:3000/\"\n"),
            "{text}"
        );
        assert_eq!(store.load_profile(&name).unwrap().web.unwrap().url, url);
    }

    #[test]
    fn ports_must_be_1_to_65535() {
        assert_eq!(parse_port("8080"), Ok(8080));
        for bad in ["0", "65536", "-1", "http", ""] {
            assert!(parse_port(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_port_in_use_is_an_error_naming_the_way_out() {
        let taken = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        let error = listen(port).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        let message = listen_error(&error, &"home".parse().unwrap(), port).to_string();
        assert!(message.contains("--port"), "{message}");
    }

    /// A "web server" on 127.0.0.1 that sends each connection's first
    /// request head back unchanged, then closes. Returns its port and how
    /// many connections it has had.
    async fn echo_server() -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
        let server = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = server.local_addr().unwrap().port();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = server.accept().await.unwrap();
                seen.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buffer = [0; 1024];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buffer[..n]),
                        }
                    }
                    let _ = stream.write_all(&head).await;
                });
            }
        });
        (port, count)
    }

    fn request(host: &str) -> Vec<u8> {
        format!("GET /alice/app HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\n\r\n").into_bytes()
    }

    /// Sends `request` to the forward on `port` and returns everything that
    /// comes back before it closes.
    async fn exchange(port: u16, request: &[u8]) -> Vec<u8> {
        let mut client = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        client.write_all(request).await.unwrap();
        let mut reply = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut reply))
            .await
            .expect("the forward should answer and close")
            .unwrap();
        reply
    }

    #[test]
    fn requests_for_localhost_are_forwarded_unchanged() {
        runtime().block_on(async {
            let (server_port, connections) = echo_server().await;
            let target = SocketAddr::from(([10, 0, 0, 1], server_port));
            let listeners = listen(0).unwrap();
            let port = listeners[0].local_addr().unwrap().port();
            let _forwards = start_forwards(listeners, Arc::new(Loopback), target).unwrap();

            // Each connection gets its own way through.
            for host in [format!("localhost:{port}"), format!("127.0.0.1:{port}")] {
                let request = request(&host);
                assert_eq!(exchange(port, &request).await, request);
            }
            assert_eq!(connections.load(Ordering::SeqCst), 2);
        });
    }

    #[test]
    fn a_request_for_another_name_is_refused_without_using_the_tunnel() {
        runtime().block_on(async {
            let (server_port, connections) = echo_server().await;
            let target = SocketAddr::from(([10, 0, 0, 1], server_port));
            let listeners = listen(0).unwrap();
            let port = listeners[0].local_addr().unwrap().port();
            let _forwards = start_forwards(listeners, Arc::new(Loopback), target).unwrap();

            let reply = exchange(port, &request(&format!("evil.example:{port}"))).await;
            let reply = String::from_utf8(reply).unwrap();
            assert!(reply.starts_with("HTTP/1.1 421 "), "{reply}");
            assert_eq!(connections.load(Ordering::SeqCst), 0);
        });
    }

    #[test]
    fn a_connection_the_tunnel_cannot_make_is_closed() {
        runtime().block_on(async {
            // Nothing listens at the target's port.
            let unused = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let target = SocketAddr::from(([10, 0, 0, 1], unused.local_addr().unwrap().port()));
            drop(unused);

            let listeners = listen(0).unwrap();
            let port = listeners[0].local_addr().unwrap().port();
            let _forwards = start_forwards(listeners, Arc::new(Loopback), target).unwrap();

            let mut client = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            client
                .write_all(&request(&format!("localhost:{port}")))
                .await
                .unwrap();
            let mut buffer = Vec::new();
            let read =
                tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut buffer))
                    .await
                    .expect("the local connection should be closed, not left hanging");
            assert!(read.is_err() || buffer.is_empty(), "{read:?}");
        });
    }
}
