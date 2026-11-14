//! Borrowing the tunnel of a `gitaway tunnel` that's already running for
//! the profile.
//!
//! The server keeps one session per device key, so two gitaway processes
//! can't each open the profile's tunnel. While `gitaway tunnel` runs, it
//! holds the profile's lock and runs a proxy for other gitaway processes
//! (see [`crate::proxy::Proxy::start_shared`]); its runs file says where
//! (see [`crate::config::run_file`]). [`SharedTunnel`] is a [`Transport`]
//! that connects through that proxy, so everything above `Transport` works
//! as it does with a tunnel of its own.
//!
//! ```text
//!  gitaway push                                   gitaway tunnel
//!  git ─▶ per-command proxy ─▶ SharedTunnel ─SOCKS (token, "gitaway-tunnel")─▶ shared proxy
//!                                                  10.0.0.1:2222              │ in its [hosts]?
//!                                                                             ▼
//!                                                                 its WireGuard tunnel
//! ```
//!
//! The running tunnel only connects addresses inside the tunnel that its own
//! `[hosts]` lists, as they were when it started.

use std::net::SocketAddr;
use std::time::Duration;

use zeroize::Zeroizing;

use super::{CONNECT_TIMEOUT, ConnectFuture, Stream, Transport, TransportError};
use crate::config::run_file::RunFile;
use crate::proxy::socks::{self, Reply};

/// How long to wait for the running tunnel to answer: its own connection
/// through the tunnel may take [`CONNECT_TIMEOUT`].
const ANSWER_TIMEOUT: Duration = Duration::from_secs(CONNECT_TIMEOUT.as_secs() + 5);

/// A running `gitaway tunnel`'s tunnel, used through its proxy for other
/// gitaway processes.
#[derive(Debug)]
pub struct SharedTunnel {
    run: RunFile,
}

impl SharedTunnel {
    /// Uses the tunnel whose proxy `run` describes.
    pub fn new(run: RunFile) -> Self {
        Self { run }
    }
}

impl Transport for SharedTunnel {
    fn connect(&self, target: SocketAddr) -> ConnectFuture<'_> {
        let proxy = self.run.proxy();
        let token = Zeroizing::new(self.run.token().to_owned());
        Box::pin(async move {
            // The SOCKS client is blocking (ssh's helper uses it too), so it
            // runs on a thread made for blocking work.
            let answer = tokio::task::spawn_blocking(move || {
                socks::connect_tunnel_address(proxy, &token, target, ANSWER_TIMEOUT)
            })
            .await
            .map_err(|e| TransportError::Setup(e.to_string()))?;
            let stream = match answer {
                Ok(Ok(stream)) => stream,
                Ok(Err(code)) => return Err(refused(target, code)),
                Err(e) => {
                    return Err(TransportError::RunningTunnel {
                        target,
                        reason: format!("it didn't answer ({e}); has it stopped?"),
                    });
                }
            };
            let stream = stream
                .set_nonblocking(true)
                .and_then(|()| tokio::net::TcpStream::from_std(stream))
                .map_err(|source| TransportError::Connect { target, source })?;
            Ok(Box::new(stream) as Box<dyn Stream>)
        })
    }
}

/// The error for a proxy reply other than success.
fn refused(target: SocketAddr, code: u8) -> TransportError {
    if code == Reply::NotAllowed as u8 {
        TransportError::NotInRunningTunnel { target }
    } else if code == Reply::ConnectionRefused as u8 {
        TransportError::Connect {
            target,
            source: std::io::ErrorKind::ConnectionRefused.into(),
        }
    } else {
        TransportError::RunningTunnel {
            target,
            reason: Reply::describe(code).to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::Profile;
    use crate::proxy::Proxy;
    use std::net::Ipv4Addr;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A "tunnel" that connects to 127.0.0.1 on the target's port instead.
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

    /// A shared proxy, as `gitaway tunnel` runs it, for a profile mapping
    /// 192.0.2.10 to 10.0.0.1.
    async fn running_tunnel() -> Proxy {
        let profile = Profile::from_toml(
            "[transport.wireguard]\nendpoint = \"vpn.example.net:51820\"\n\
             server_public_key = \"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"\n\
             [hosts]\n\"192.0.2.10\" = \"10.0.0.1\"\n",
        )
        .unwrap();
        Proxy::start_shared(Arc::new(Loopback), profile.hosts)
            .await
            .unwrap()
    }

    fn borrow(proxy: &Proxy) -> SharedTunnel {
        SharedTunnel::new(RunFile::new(proxy.address(), proxy.token(), "wireguard"))
    }

    #[test]
    fn bytes_cross_the_borrowed_tunnel_both_ways() {
        runtime().block_on(async {
            let server = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let port = server.local_addr().unwrap().port();
            tokio::spawn(async move {
                let (mut stream, _) = server.accept().await.unwrap();
                let mut ping = [0; 4];
                stream.read_exact(&mut ping).await.unwrap();
                stream.write_all(b"pong").await.unwrap();
            });
            let proxy = running_tunnel().await;

            let mut stream = borrow(&proxy)
                .connect(SocketAddr::from(([10, 0, 0, 1], port)))
                .await
                .unwrap();
            stream.write_all(b"ping").await.unwrap();
            let mut pong = [0; 4];
            stream.read_exact(&mut pong).await.unwrap();
            assert_eq!(&pong, b"pong");
        });
    }

    #[test]
    fn an_address_outside_its_hosts_says_to_restart_it() {
        runtime().block_on(async {
            let proxy = running_tunnel().await;
            let error = borrow(&proxy)
                .connect(SocketAddr::from(([10, 0, 0, 2], 22)))
                .await
                .err()
                .unwrap();
            assert!(
                matches!(error, TransportError::NotInRunningTunnel { .. }),
                "{error}"
            );
        });
    }

    #[test]
    fn a_wrong_token_or_a_stopped_tunnel_is_an_error() {
        runtime().block_on(async {
            let proxy = running_tunnel().await;
            let target = SocketAddr::from(([10, 0, 0, 1], 22));
            let guessed =
                SharedTunnel::new(RunFile::new(proxy.address(), &"0".repeat(64), "wireguard"));
            assert!(guessed.connect(target).await.is_err());

            let stopped = borrow(&proxy);
            drop(proxy);
            let error = stopped.connect(target).await.err().unwrap();
            assert!(
                matches!(error, TransportError::RunningTunnel { .. }),
                "{error}"
            );
        });
    }
}
