//! Decide whether you're on a profile's home network by making its SSH
//! server prove its identity.
//!
//! Reaching an address proves nothing: another network (a café, an office)
//! can use the same private addresses as your home. So gitaway connects to
//! the profile's `[home].ssh` address and runs the first part of an SSH
//! connection, the key exchange, where the server must sign the exchange with
//! its private host key. Only the real server can produce that signature.
//! gitaway then checks that the key is the one pinned in the profile, and
//! disconnects before any login.
//!
//! ```text
//! gitaway                                      [home].ssh
//!   |---- TCP connect (gives up after 200 ms) ---->|
//!   |<---------- SSH key exchange ---------------->|  server signs with its
//!   |  signature valid?        (russh checks)      |  private host key
//!   |  key == profile's host_key?  (we check)      |
//!   '-- disconnect, no login                       |
//! ```
//!
//! # Security notes
//!
//! - russh verifies the server's signature *before* it calls
//!   [`russh::client::Handler::check_server_key`] (checked in the 0.65.1
//!   source, and stated in its docs). A key only reaches us once its holder
//!   has proved it owns it.
//! - The client offers only the pinned key's type, so a server with several
//!   host keys must present the pinned one, and SHA-1 RSA signatures are
//!   never accepted.
//! - SSH certificates are never requested, and rejected if presented.
//! - Every failure, including a timeout, means "away". It never means "home".
//!
//! # Extending
//!
//! HTTPS-only servers (pinning a TLS certificate instead) would be a second
//! verifier with the same [`Verdict`] type, chosen by the profile.

use std::borrow::Cow;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use russh::client;
use russh::keys::{Algorithm, HashAlg, PublicKeyOrCertificate};
use russh::{Preferred, SshId};
use tokio::net::TcpStream;

use crate::config::profile::Home;
use crate::config::value::HostKey;

/// How long each stage of the check may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    /// Name lookup plus TCP connect. Away from home this is usually the whole
    /// cost of the check, so it's short.
    pub connect: Duration,
    /// The SSH key exchange, once connected. Longer, so a slow home server
    /// still has time to answer.
    pub exchange: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_millis(200),
            exchange: Duration::from_secs(1),
        }
    }
}

/// The result of a home check.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verdict {
    /// The server proved it holds the pinned host key.
    Home,
    /// Anything else, with the reason.
    Away(AwayReason),
}

/// Why a home check decided you're away.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AwayReason {
    /// The connection was refused, or there's no route or no such host.
    #[error("couldn't connect: {0}")]
    Unreachable(String),
    /// Nothing answered in time.
    #[error("no answer within {} ms", .0.as_millis())]
    ConnectTimeout(Duration),
    /// Something answered, but the key exchange didn't finish in time.
    #[error("the SSH key exchange didn't finish within {} ms", .0.as_millis())]
    ExchangeTimeout(Duration),
    /// Something answered, but not with a valid SSH key exchange (including
    /// a bad signature).
    #[error("the SSH key exchange failed: {0}")]
    Handshake(String),
    /// The server offered a certificate instead of a plain host key.
    #[error("the server presented an SSH certificate; gitaway pins plain host keys")]
    Certificate,
    /// A real SSH server answered, proving it holds a *different* key. On a
    /// foreign network that's another machine at the same address; at home it
    /// means the server's key changed. Worth telling the user either way.
    #[error("a server answered with a different host key ({fingerprint})")]
    WrongKey {
        /// The fingerprint of the key it presented, for display.
        fingerprint: String,
    },
}

/// The check couldn't run at all. Distinct from [`Verdict::Away`], which
/// means it ran and the answer was no.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CheckError {
    /// The async runtime couldn't start.
    #[error("couldn't start the home check: {0}")]
    Runtime(io::Error),
}

/// Runs the home check with the default timeouts.
///
/// This blocks the calling thread. Don't call it from inside an async
/// runtime: use [`verify`] there instead.
pub fn check(home: &Home) -> Result<Verdict, CheckError> {
    check_with(home, Timeouts::default())
}

/// Runs the home check with the given timeouts. See [`check`].
pub fn check_with(home: &Home, timeouts: Timeouts) -> Result<Verdict, CheckError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(CheckError::Runtime)?;
    let verdict = runtime.block_on(verify(home, timeouts));
    // A name lookup that timed out may still be running on a background
    // thread. Don't wait for it.
    runtime.shutdown_background();
    Ok(verdict)
}

/// The home check, for callers already inside a tokio runtime.
pub async fn verify(home: &Home, timeouts: Timeouts) -> Verdict {
    let address = (home.ssh.host.as_str(), home.ssh_port());
    let stream = match tokio::time::timeout(timeouts.connect, TcpStream::connect(address)).await {
        Err(_) => return Verdict::Away(AwayReason::ConnectTimeout(timeouts.connect)),
        Ok(Err(e)) => return Verdict::Away(AwayReason::Unreachable(e.to_string())),
        Ok(Ok(stream)) => stream,
    };

    let seen = Arc::new(Mutex::new(None));
    let handler = KeyRecorder {
        seen: Arc::clone(&seen),
    };
    let exchange = client::connect_stream(client_config(&home.host_key), stream, handler);
    let outcome = tokio::time::timeout(timeouts.exchange, exchange).await;
    let seen = seen.lock().unwrap_or_else(PoisonError::into_inner).take();

    match (outcome, seen) {
        (Err(_), _) => Verdict::Away(AwayReason::ExchangeTimeout(timeouts.exchange)),
        // Our handler always stops the connection, so `outcome` is an error
        // whenever a key was seen. What was seen decides.
        (Ok(_), Some(Seen::Key(key))) => {
            if home.host_key.matches(&key) {
                Verdict::Home
            } else {
                Verdict::Away(AwayReason::WrongKey {
                    fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
                })
            }
        }
        (Ok(_), Some(Seen::Certificate)) => Verdict::Away(AwayReason::Certificate),
        (Ok(Err(e)), None) => Verdict::Away(AwayReason::Handshake(e.to_string())),
        (Ok(Ok(_)), None) => Verdict::Away(AwayReason::Handshake(
            "the server's key was never checked".to_owned(),
        )),
    }
}

/// Client settings: identify as gitaway (so server logs say what connected),
/// offer only the pinned key's type, and never ask for certificates.
fn client_config(pinned: &HostKey) -> Arc<client::Config> {
    let preferred = Preferred {
        key: Cow::Owned(signature_algorithms(pinned.algorithm())),
        host_key_certificates: Cow::Borrowed(&[]),
        ..Preferred::DEFAULT
    };
    Arc::new(client::Config {
        client_id: SshId::Standard(Cow::Borrowed(concat!(
            "SSH-2.0-gitaway_",
            env!("CARGO_PKG_VERSION")
        ))),
        preferred,
        ..client::Config::default()
    })
}

/// The SSH signature algorithms to offer for a host key of this type. RSA
/// keys can sign with SHA-512 or SHA-256; SHA-1 (`ssh-rsa`) is deliberately
/// left out.
fn signature_algorithms(key_type: Algorithm) -> Vec<Algorithm> {
    match key_type {
        Algorithm::Rsa { .. } => vec![
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha512),
            },
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha256),
            },
        ],
        other => vec![other],
    }
}

/// What the server presented, recorded after russh verified the signature.
enum Seen {
    Key(russh::keys::PublicKey),
    Certificate,
}

/// A russh client handler that records the server's key and then stops.
struct KeyRecorder {
    seen: Arc<Mutex<Option<Seen>>>,
}

impl client::Handler for KeyRecorder {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let seen = match server_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => Seen::Key(key.clone()),
            PublicKeyOrCertificate::Certificate(_) => Seen::Certificate,
        };
        *self.seen.lock().unwrap_or_else(PoisonError::into_inner) = Some(seen);
        // Always stop here: the verdict comes from what was recorded, and
        // gitaway never wants to go on to log in.
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::HostPort;
    use russh::keys::PrivateKey;
    use russh::keys::ssh_key::private::Ed25519Keypair;
    use std::time::Instant;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    fn ed25519(seed: u8) -> PrivateKey {
        PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
    }

    fn home(port: u16, pinned: &PrivateKey) -> Home {
        Home {
            ssh: HostPort {
                host: "127.0.0.1".into(),
                port: Some(port),
            },
            host_key: HostKey::from(pinned.public_key().clone()),
        }
    }

    fn fast() -> Timeouts {
        Timeouts {
            connect: Duration::from_millis(500),
            exchange: Duration::from_millis(500),
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// An SSH server on 127.0.0.1 with the given host keys. Returns its port.
    async fn ssh_server(keys: Vec<PrivateKey>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let config = Arc::new(russh::server::Config {
            keys,
            ..Default::default()
        });
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let config = Arc::clone(&config);
                tokio::spawn(async move {
                    if let Ok(session) =
                        russh::server::run_stream(config, socket, super::tests_support::Server)
                            .await
                    {
                        let _ = session.await;
                    }
                });
            }
        });
        port
    }

    /// A TCP server that runs `respond` on each connection.
    async fn tcp_server<F, Fut>(respond: F) -> u16
    where
        F: Fn(TcpStream) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(respond(socket));
            }
        });
        port
    }

    #[test]
    fn the_pinned_server_is_home() {
        runtime().block_on(async {
            let key = ed25519(1);
            let port = ssh_server(vec![key.clone()]).await;
            assert_eq!(verify(&home(port, &key), fast()).await, Verdict::Home);
        });
    }

    #[test]
    fn a_server_with_another_key_is_away_and_named() {
        runtime().block_on(async {
            let (real, pinned) = (ed25519(1), ed25519(2));
            let port = ssh_server(vec![real.clone()]).await;
            let expected = real.public_key().fingerprint(HashAlg::Sha256).to_string();
            assert_eq!(
                verify(&home(port, &pinned), fast()).await,
                Verdict::Away(AwayReason::WrongKey {
                    fingerprint: expected
                })
            );
        });
    }

    #[test]
    fn a_closed_port_is_away() {
        runtime().block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            let verdict = verify(&home(port, &ed25519(1)), fast()).await;
            assert!(
                matches!(verdict, Verdict::Away(AwayReason::Unreachable(_))),
                "{verdict:?}"
            );
        });
    }

    #[test]
    fn a_silent_server_times_out_as_away() {
        runtime().block_on(async {
            let port = tcp_server(|socket| async move {
                // Hold the connection open without ever speaking.
                let _socket = socket;
                std::future::pending::<()>().await;
            })
            .await;
            let timeouts = Timeouts {
                connect: Duration::from_millis(500),
                exchange: Duration::from_millis(100),
            };
            assert_eq!(
                verify(&home(port, &ed25519(1)), timeouts).await,
                Verdict::Away(AwayReason::ExchangeTimeout(timeouts.exchange))
            );
        });
    }

    #[test]
    fn a_server_that_is_not_ssh_is_away() {
        runtime().block_on(async {
            let port = tcp_server(|mut socket| async move {
                let _ = socket.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
            })
            .await;
            let verdict = verify(&home(port, &ed25519(1)), fast()).await;
            assert!(
                matches!(verdict, Verdict::Away(AwayReason::Handshake(_))),
                "{verdict:?}"
            );
        });
    }

    #[test]
    fn an_unroutable_address_is_away_within_the_timeout() {
        let mut target = home(9, &ed25519(1));
        // A documentation address: nothing answers, whether or not a route exists.
        target.ssh.host = "192.0.2.1".into();
        let started = Instant::now();
        let verdict = check_with(&target, Timeouts::default()).unwrap();
        assert!(matches!(verdict, Verdict::Away(_)), "{verdict:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    /// The blocking wrapper works from ordinary code, with the server on
    /// another thread.
    #[test]
    fn check_with_blocks_until_done() {
        let key = ed25519(3);
        let (port_tx, port_rx) = std::sync::mpsc::channel();
        let server_key = key.clone();
        std::thread::spawn(move || {
            runtime().block_on(async move {
                port_tx.send(ssh_server(vec![server_key]).await).unwrap();
                std::future::pending::<()>().await;
            });
        });
        let port = port_rx.recv().unwrap();
        assert_eq!(
            check_with(&home(port, &key), fast()).unwrap(),
            Verdict::Home
        );
    }

    #[test]
    fn rsa_pins_offer_only_sha2_signatures() {
        let offered = signature_algorithms(Algorithm::Rsa { hash: None });
        assert!(!offered.contains(&Algorithm::Rsa { hash: None }));
        assert!(offered.contains(&Algorithm::Rsa {
            hash: Some(HashAlg::Sha512)
        }));
        assert_eq!(
            signature_algorithms(Algorithm::Ed25519),
            [Algorithm::Ed25519]
        );
    }
}

/// Tests that need OpenSSH's `ssh-keygen`, to prove interoperability with the
/// keys and fingerprints users will actually paste.
#[cfg(all(test, unix))]
mod openssh_tests {
    use super::*;
    use crate::address::HostPort;
    use std::process::Command;
    use tokio::net::TcpListener;

    /// Generates a key with ssh-keygen. Returns (private key, public key line).
    fn keygen(dir: &std::path::Path, kind: &str) -> (russh::keys::PrivateKey, String) {
        let path = dir.join(kind);
        let status = Command::new("ssh-keygen")
            .args(["-q", "-t", kind, "-N", "", "-C", "test", "-f"])
            .arg(&path)
            .status()
            .expect("ssh-keygen is needed for these tests");
        assert!(status.success());
        let private = russh::keys::load_secret_key(&path, None).unwrap();
        let public = std::fs::read_to_string(path.with_extension("pub")).unwrap();
        (private, public)
    }

    fn fingerprint_from_ssh_keygen(public_key_file: &std::path::Path) -> String {
        let out = Command::new("ssh-keygen")
            .arg("-lf")
            .arg(public_key_file)
            .output()
            .unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        text.split_whitespace().nth(1).unwrap().to_owned()
    }

    #[test]
    fn fingerprints_match_ssh_keygen() {
        let dir = tempfile::TempDir::new().unwrap();
        for kind in ["ed25519", "ecdsa", "rsa"] {
            let (_, public) = keygen(dir.path(), kind);
            let key: HostKey = public.parse().unwrap();
            assert_eq!(
                key.fingerprint(),
                fingerprint_from_ssh_keygen(&dir.path().join(format!("{kind}.pub"))),
                "{kind}"
            );
        }
    }

    /// A server with several host keys presents the one the client asks for
    /// first. Pinning each key in turn must still verify, which only works
    /// because the client offers just the pinned key's type.
    #[test]
    fn the_pinned_key_type_is_requested_from_multi_key_servers() {
        let dir = tempfile::TempDir::new().unwrap();
        let keys: Vec<_> = ["ed25519", "ecdsa", "rsa"]
            .into_iter()
            .map(|kind| keygen(dir.path(), kind))
            .collect();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let config = Arc::new(russh::server::Config {
                keys: keys.iter().map(|(private, _)| private.clone()).collect(),
                ..Default::default()
            });
            tokio::spawn(async move {
                while let Ok((socket, _)) = listener.accept().await {
                    let config = Arc::clone(&config);
                    tokio::spawn(async move {
                        if let Ok(session) =
                            russh::server::run_stream(config, socket, super::tests_support::Server)
                                .await
                        {
                            let _ = session.await;
                        }
                    });
                }
            });

            for (_, public) in &keys {
                let home = Home {
                    ssh: HostPort {
                        host: "127.0.0.1".into(),
                        port: Some(port),
                    },
                    host_key: public.parse().unwrap(),
                };
                let timeouts = Timeouts {
                    connect: Duration::from_millis(500),
                    exchange: Duration::from_secs(2),
                };
                assert_eq!(verify(&home, timeouts).await, Verdict::Home, "{public}");
            }
        });
    }
}

#[cfg(test)]
mod tests_support {
    /// A russh server handler with every default: enough for a key exchange.
    pub struct Server;
    impl russh::server::Handler for Server {
        type Error = russh::Error;
    }
}
