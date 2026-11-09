//! The WireGuard transport: Mullvad's `gotatun` for the protocol, and
//! [`super::netstack`] for TCP.
//!
//! ```text
//!  connect(10.0.0.1:2222)
//!       │
//!  NetStack (smoltcp)  ── IP packets ──▶ ToGotatun / FromStack (channels)
//!                                              │
//!  gotatun Device: handshake, timers, encryption
//!                                              │  UDP
//!                                              ▼
//!                                   the profile's endpoint
//! ```
//!
//! Everything runs inside the gitaway process: no virtual network adapter,
//! no root, and no other program ever sees the private key. The WireGuard
//! handshake happens on the first packet, so opening the transport is quick
//! and the first [`Transport::connect`] includes the handshake.

use std::io;
use std::iter;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use bytes::BytesMut;
use gotatun::device::{Device, DeviceBuilder, Peer};
use gotatun::packet::{Ip, Packet, PacketBufPool};
use gotatun::tun::{IpRecv, IpSend, MtuWatcher};
use gotatun::udp::socket::UdpSocketFactory;
use gotatun::x25519;
use ipnetwork::IpNetwork;
use tokio::sync::mpsc;

use super::netstack::{NetStack, PacketPipe};
use super::{CONNECT_TIMEOUT, ConnectFuture, Stream, Transport, TransportError};
use crate::config::profile::WireGuard;
use crate::keys::device_key::DeviceKey;

/// WireGuard's usual MTU: fits a 1500-byte link after WireGuard's overhead
/// on IPv6.
pub const DEFAULT_MTU: u16 = 1420;

/// How long to wait for the endpoint's DNS lookup.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

type WgDevice = Device<(UdpSocketFactory, ToStack, FromStack)>;

/// An open WireGuard tunnel. Dropping it closes the tunnel.
pub struct WireGuardTransport {
    stack: NetStack,
    // Held to keep the tunnel running; dropping it stops gotatun's tasks.
    _device: WgDevice,
}

impl WireGuardTransport {
    /// Starts the tunnel for profile `name`. The device key is handed to
    /// gotatun and wiped when the tunnel closes.
    pub async fn open(
        name: &str,
        settings: &WireGuard,
        key: DeviceKey,
    ) -> Result<Self, TransportError> {
        let address = settings
            .address
            .as_ref()
            .ok_or_else(|| TransportError::NoAddress {
                profile: name.to_owned(),
            })?;
        let endpoint = resolve_endpoint(settings).await?;
        let server_key = x25519::PublicKey::from(settings.server_public_key.to_bytes());
        let mtu = settings.mtu.unwrap_or(DEFAULT_MTU);
        Self::start(address.ip, mtu, key, server_key, endpoint).await
    }

    async fn start(
        address: IpAddr,
        mtu: u16,
        key: DeviceKey,
        server_key: x25519::PublicKey,
        endpoint: SocketAddr,
    ) -> Result<Self, TransportError> {
        let (stack, pipe) = NetStack::new(address, usize::from(mtu));
        let (to_stack, from_stack) = split(pipe, mtu);
        let peer = Peer::new(server_key)
            .with_endpoint(endpoint)
            .with_allowed_ips(everything());
        let device = DeviceBuilder::new()
            .with_default_udp()
            .with_ip_pair(to_stack, from_stack)
            .with_private_key(key.into_secret())
            .with_peer(peer)
            .build()
            .await
            .map_err(|e| TransportError::Setup(e.to_string()))?;
        Ok(Self {
            stack,
            _device: device,
        })
    }
}

impl Transport for WireGuardTransport {
    fn connect(&self, target: SocketAddr) -> ConnectFuture<'_> {
        Box::pin(async move {
            let stream = tokio::time::timeout(CONNECT_TIMEOUT, self.stack.connect(target))
                .await
                .map_err(|_| TransportError::Timeout { target })?
                .map_err(|source| TransportError::Connect { target, source })?;
            Ok(Box::new(stream) as Box<dyn Stream>)
        })
    }
}

/// Looks up the endpoint's address. A name may have several; the first is
/// used, as `wg` does.
async fn resolve_endpoint(settings: &WireGuard) -> Result<SocketAddr, TransportError> {
    let endpoint = &settings.endpoint;
    let port = endpoint.port.expect("validated: endpoints have a port");
    let failed = |reason: String| TransportError::Endpoint {
        endpoint: endpoint.to_string(),
        reason,
    };
    let lookup = tokio::net::lookup_host((endpoint.host.as_str(), port));
    let mut addresses = tokio::time::timeout(LOOKUP_TIMEOUT, lookup)
        .await
        .map_err(|_| failed("the lookup timed out".to_owned()))?
        .map_err(|e| failed(e.to_string()))?;
    addresses
        .next()
        .ok_or_else(|| failed("it has no addresses".to_owned()))
}

/// Every address goes through the tunnel; which ones gitaway actually
/// connects to is limited by the proxy, not here.
fn everything() -> [IpNetwork; 2] {
    [
        IpNetwork::new(Ipv4Addr::UNSPECIFIED.into(), 0).expect("valid network"),
        IpNetwork::new(Ipv6Addr::UNSPECIFIED.into(), 0).expect("valid network"),
    ]
}

/// Turns the stack's packet channels into gotatun's two IP interfaces.
fn split(pipe: PacketPipe, mtu: u16) -> (ToStack, FromStack) {
    (
        ToStack(pipe.inbound),
        FromStack {
            packets: pipe.outbound,
            mtu,
        },
    )
}

/// Decrypted packets from gotatun, into the stack.
struct ToStack(mpsc::Sender<Vec<u8>>);

/// Packets from the stack, for gotatun to encrypt and send.
struct FromStack {
    packets: mpsc::Receiver<Vec<u8>>,
    mtu: u16,
}

impl IpSend for ToStack {
    async fn send(&mut self, packet: Packet<Ip>) -> io::Result<()> {
        self.0
            .send(packet.into_bytes().to_vec())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the network stack stopped"))
    }
}

impl IpRecv for FromStack {
    async fn recv<'a>(
        &'a mut self,
        _pool: &mut PacketBufPool,
    ) -> io::Result<impl Iterator<Item = Packet<Ip>> + Send + 'a> {
        loop {
            let Some(bytes) = self.packets.recv().await else {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the network stack stopped",
                ));
            };
            // smoltcp only builds valid IP packets, but never hand gotatun
            // something it can't parse.
            if let Ok(packet) = Packet::from_bytes(BytesMut::from(&bytes[..])).try_into_ip() {
                return Ok(iter::once(packet));
            }
        }
    }

    fn mtu(&self) -> MtuWatcher {
        MtuWatcher::new(self.mtu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const CLIENT_ADDRESS: &str = "10.0.0.2";
    const SERVER_ADDRESS: &str = "10.0.0.1";

    /// A WireGuard server on 127.0.0.1 that echoes one TCP connection on
    /// `port`, as the home network would serve git. Returns its UDP port.
    struct EchoServer {
        udp_port: u16,
        public_key: x25519::PublicKey,
        _device: WgDevice,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for EchoServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn echo_server(client: &x25519::PublicKey, port: u16) -> EchoServer {
        let key = DeviceKey::generate();
        let public_key = x25519::PublicKey::from(key.public_key().to_bytes());
        let (stack, pipe) = NetStack::new(SERVER_ADDRESS.parse().unwrap(), 1420);
        let (to_stack, from_stack) = split(pipe, 1420);
        let peer = Peer::new(*client)
            .with_allowed_ip(IpNetwork::new(CLIENT_ADDRESS.parse().unwrap(), 32).unwrap());
        let device = DeviceBuilder::new()
            .with_default_udp()
            .with_ip_pair(to_stack, from_stack)
            .with_private_key(key.into_secret())
            .with_peer(peer)
            .build()
            .await
            .unwrap();
        let udp_port = device.read(async |d| d.listen_port()).await;
        let task = tokio::spawn(async move {
            let mut stream = stack.accept(port).await.unwrap();
            let mut buf = vec![0; 64 * 1024];
            loop {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => stream.write_all(&buf[..n]).await.unwrap(),
                }
            }
            stream.shutdown().await.unwrap();
            std::future::pending::<()>().await;
        });
        EchoServer {
            udp_port,
            public_key,
            _device: device,
            task,
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn localhost(port: u16) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::LOCALHOST, port))
    }

    #[test]
    fn data_crosses_a_real_wireguard_tunnel() {
        runtime().block_on(async {
            let client_key = DeviceKey::generate();
            let client_public = x25519::PublicKey::from(client_key.public_key().to_bytes());
            let server = echo_server(&client_public, 2222).await;

            let transport = WireGuardTransport::start(
                CLIENT_ADDRESS.parse().unwrap(),
                1420,
                client_key,
                server.public_key,
                localhost(server.udp_port),
            )
            .await
            .unwrap();

            let target: SocketAddr = format!("{SERVER_ADDRESS}:2222").parse().unwrap();
            let mut stream = transport.connect(target).await.unwrap();
            let message: Vec<u8> = (0..300_000).map(|i| (i % 253) as u8).collect();
            let (mut read_half, mut write_half) = tokio::io::split(&mut stream);
            let writer = async {
                write_half.write_all(&message).await.unwrap();
                write_half.shutdown().await.unwrap();
            };
            let mut echoed = Vec::new();
            let reader = read_half.read_to_end(&mut echoed);
            let ((), read) = tokio::join!(writer, reader);
            read.unwrap();
            assert!(echoed == message, "echoed data differs");
        });
    }

    #[test]
    fn a_server_that_does_not_know_this_device_times_out() {
        runtime().block_on(async {
            // The server expects a different device's key.
            let stranger = x25519::PublicKey::from(DeviceKey::generate().public_key().to_bytes());
            let server = echo_server(&stranger, 2222).await;

            let transport = WireGuardTransport::start(
                CLIENT_ADDRESS.parse().unwrap(),
                1420,
                DeviceKey::generate(),
                server.public_key,
                localhost(server.udp_port),
            )
            .await
            .unwrap();
            tokio::time::pause();
            let target: SocketAddr = format!("{SERVER_ADDRESS}:2222").parse().unwrap();
            let error = transport.connect(target).await.err().unwrap();
            assert!(matches!(error, TransportError::Timeout { .. }), "{error}");
        });
    }

    #[test]
    fn a_profile_without_an_address_is_refused_with_advice() {
        runtime().block_on(async {
            let settings = WireGuard {
                endpoint: "vpn.example.net:51820".parse().unwrap(),
                server_public_key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
                    .parse()
                    .unwrap(),
                address: None,
                mtu: None,
            };
            let error = WireGuardTransport::open("home", &settings, DeviceKey::generate())
                .await
                .err()
                .unwrap();
            let message = error.to_string();
            assert!(
                matches!(error, TransportError::NoAddress { .. }),
                "{message}"
            );
            assert!(message.contains("gitaway profile set home"), "{message}");
        });
    }
}
