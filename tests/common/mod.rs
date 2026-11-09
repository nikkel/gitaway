//! Helpers shared by the integration tests.

#![allow(dead_code)] // each test file uses a different subset

use std::sync::Arc;

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
