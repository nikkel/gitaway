//! Helpers shared by the integration tests.

#![allow(dead_code)] // each test file uses a different subset

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use russh::keys::PrivateKey;
use russh::keys::ssh_key::private::Ed25519Keypair;

struct Server;
impl russh::server::Handler for Server {
    type Error = russh::Error;
}

/// Starts an SSH server on 127.0.0.1 with a host key from `seed`.
/// Returns its port and public key line.
pub fn ssh_server(seed: u8) -> (u16, String) {
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
                    if let Ok(session) = russh::server::run_stream(config, socket, Server).await {
                        let _ = session.await;
                    }
                });
            }
        });
    });
    (rx.recv().unwrap(), public)
}

/// A WireGuard server standing in for a home network, on 127.0.0.1.
pub struct HomeServer {
    /// The UDP port to use as the profile's endpoint.
    pub udp_port: u16,
}

/// The server's address inside the tunnel.
pub const HOME_TUNNEL_ADDRESS: &str = "10.0.0.1";
/// The device's address inside the tunnel.
pub const DEVICE_TUNNEL_ADDRESS: &str = "10.0.0.2";

/// Starts a WireGuard server with `key` that accepts only `device` (a
/// WireGuard public key). TCP connections through the tunnel to
/// 10.0.0.1:<port> are forwarded to 127.0.0.1:<port> for each of `ports`,
/// as if those servers were on the home network. Runs until the test ends.
pub fn home_server(
    key: gitaway::keys::device_key::DeviceKey,
    device: &gitaway::config::value::WireGuardPublicKey,
    ports: &[u16],
) -> HomeServer {
    use gitaway::transport::wireguard::Tunnel;
    use ipnetwork::IpNetwork;

    let device = device.clone();
    let ports = ports.to_vec();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let allowed = [IpNetwork::new(DEVICE_TUNNEL_ADDRESS.parse().unwrap(), 32).unwrap()];
            let tunnel = Tunnel::start(
                HOME_TUNNEL_ADDRESS.parse().unwrap(),
                1420,
                key,
                &device,
                None,
                &allowed,
                0,
            )
            .await
            .unwrap();
            tx.send(tunnel.listen_port().await).unwrap();
            let tunnel = Arc::new(tunnel);
            for port in ports {
                let tunnel = tunnel.clone();
                tokio::spawn(async move {
                    loop {
                        let Ok(mut inside) = tunnel.stack().accept(port).await else {
                            return;
                        };
                        // Accept the next connection at once, while this one is served.
                        tokio::spawn(async move {
                            if let Ok(mut local) =
                                tokio::net::TcpStream::connect(("127.0.0.1", port)).await
                            {
                                let _ =
                                    tokio::io::copy_bidirectional(&mut inside, &mut local).await;
                            }
                        });
                    }
                });
            }
            std::future::pending::<()>().await;
        });
    });
    HomeServer {
        udp_port: rx.recv().unwrap(),
    }
}

/// A minimal HTTP server answering every request with 200 and an empty
/// body, which git reads as an empty repository. Returns its port.
pub async fn empty_http_server() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            });
        }
    });
    port
}
