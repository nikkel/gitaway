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
use crate::config::value::WireGuardPublicKey;
use crate::keys::device_key::DeviceKey;

/// WireGuard's usual MTU: fits a 1500-byte link after WireGuard's overhead
/// on IPv6.
pub const DEFAULT_MTU: u16 = 1420;

/// How long to wait for the endpoint's DNS lookup.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

type WgDevice = Device<(UdpSocketFactory, ToStack, FromStack)>;

/// A WireGuard interface in userspace: gotatun on top of a [`NetStack`].
///
/// Both ends of a tunnel can be built from it. gitaway's own side is
/// [`WireGuardTransport`]; tests use it for the server's side too. Dropping
/// it closes the tunnel.
pub struct Tunnel {
    stack: NetStack,
    // Held to keep the tunnel running; dropping it stops gotatun's tasks.
    device: WgDevice,
}

impl Tunnel {
    /// Starts an interface with `address` and `key`, talking to one peer.
    /// `endpoint` is where to find the peer (a server can leave it out and
    /// learn it from the peer's handshake), and `allowed_ips` the addresses
    /// that may come from and go to it. `listen_port` 0 lets the system pick.
    pub async fn start(
        address: IpAddr,
        mtu: u16,
        key: DeviceKey,
        peer_key: &WireGuardPublicKey,
        endpoint: Option<SocketAddr>,
        allowed_ips: &[IpNetwork],
        listen_port: u16,
    ) -> Result<Self, TransportError> {
        let (stack, pipe) = NetStack::new(address, usize::from(mtu));
        let (to_stack, from_stack) = split(pipe, mtu);
        let mut peer = Peer::new(x25519::PublicKey::from(peer_key.to_bytes()))
            .with_allowed_ips(allowed_ips.iter().copied());
        if let Some(endpoint) = endpoint {
            peer = peer.with_endpoint(endpoint);
        }
        let device = DeviceBuilder::new()
            .with_default_udp()
            .with_listen_port(listen_port)
            .with_ip_pair(to_stack, from_stack)
            .with_private_key(key.into_secret())
            .with_peer(peer)
            .build()
            .await
            .map_err(|e| TransportError::Setup(e.to_string()))?;
        Ok(Self { stack, device })
    }

    /// The TCP/IP stack inside the tunnel.
    pub fn stack(&self) -> &NetStack {
        &self.stack
    }

    /// The UDP port the tunnel listens on.
    pub async fn listen_port(&self) -> u16 {
        self.device.read(async |d| d.listen_port()).await
    }

    /// Whether a handshake with the peer has completed, so packets can
    /// cross the tunnel.
    pub async fn handshake_done(&self) -> bool {
        self.device
            .read(async |d| {
                d.peers()
                    .await
                    .iter()
                    .any(|p| p.stats.last_handshake.is_some())
            })
            .await
    }
}

/// gitaway's side of a WireGuard tunnel to a profile's server.
pub struct WireGuardTransport {
    tunnel: Tunnel,
    endpoint: SocketAddr,
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
        if key.public_key() == settings.server_public_key {
            return Err(TransportError::OwnKey {
                profile: name.to_owned(),
            });
        }
        let endpoint = resolve_endpoint(settings).await?;
        let mtu = settings.mtu.unwrap_or(DEFAULT_MTU);
        Self::start(address.ip, mtu, key, &settings.server_public_key, endpoint).await
    }

    async fn start(
        address: IpAddr,
        mtu: u16,
        key: DeviceKey,
        server_key: &WireGuardPublicKey,
        endpoint: SocketAddr,
    ) -> Result<Self, TransportError> {
        let tunnel = Tunnel::start(
            address,
            mtu,
            key,
            server_key,
            Some(endpoint),
            &everything(),
            0,
        )
        .await?;
        Ok(Self { tunnel, endpoint })
    }
}

impl Transport for WireGuardTransport {
    fn connect(&self, target: SocketAddr) -> ConnectFuture<'_> {
        Box::pin(async move {
            let connecting = self.tunnel.stack.connect(target);
            let Ok(result) = tokio::time::timeout(CONNECT_TIMEOUT, connecting).await else {
                return Err(if self.tunnel.handshake_done().await {
                    TransportError::Timeout { target }
                } else {
                    TransportError::NoHandshake {
                        endpoint: self.endpoint,
                    }
                });
            };
            let stream = result.map_err(|source| TransportError::Connect { target, source })?;
            Ok(Box::new(stream) as Box<dyn Stream>)
        })
    }
}

/// Looks up the endpoint's address. See [`pick_endpoint`] for which one is
/// used when a name has several.
async fn resolve_endpoint(settings: &WireGuard) -> Result<SocketAddr, TransportError> {
    let endpoint = &settings.endpoint;
    let port = endpoint.port.expect("validated: endpoints have a port");
    let failed = |reason: String| TransportError::Endpoint {
        endpoint: endpoint.to_string(),
        reason,
    };
    let lookup = tokio::net::lookup_host((endpoint.host.as_str(), port));
    let addresses = tokio::time::timeout(LOOKUP_TIMEOUT, lookup)
        .await
        .map_err(|_| failed("the lookup timed out".to_owned()))?
        .map_err(|e| failed(e.to_string()))?;
    pick_endpoint(addresses).ok_or_else(|| failed("it has no addresses".to_owned()))
}

/// Picks the first IPv4 address, or the first address if there's none.
///
/// Many networks answer a name that only has an IPv4 address with a made-up
/// IPv6 one too (DNS64), and list it first. Using it needs working IPv6,
/// which a laptop often doesn't have, so the handshake would vanish. Home
/// VPN servers are nearly always reachable over IPv4.
fn pick_endpoint(addresses: impl IntoIterator<Item = SocketAddr>) -> Option<SocketAddr> {
    let mut first = None;
    for address in addresses {
        if address.is_ipv4() {
            return Some(address);
        }
        first.get_or_insert(address);
    }
    first
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
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const CLIENT_ADDRESS: &str = "10.0.0.2";
    const SERVER_ADDRESS: &str = "10.0.0.1";

    /// A WireGuard server on 127.0.0.1 that echoes one TCP connection on
    /// `port`, as the home network would serve git.
    struct EchoServer {
        udp_port: u16,
        public_key: WireGuardPublicKey,
        _tunnel: Arc<Tunnel>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for EchoServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn echo_server(client: &WireGuardPublicKey, port: u16) -> EchoServer {
        let key = DeviceKey::generate();
        let public_key = key.public_key();
        let client_only = [IpNetwork::new(CLIENT_ADDRESS.parse().unwrap(), 32).unwrap()];
        let tunnel = Tunnel::start(
            SERVER_ADDRESS.parse().unwrap(),
            1420,
            key,
            client,
            None,
            &client_only,
            0,
        )
        .await
        .unwrap();
        let udp_port = tunnel.listen_port().await;
        let tunnel = Arc::new(tunnel);
        let server = tunnel.clone();
        let task = tokio::spawn(async move {
            let mut stream = server.stack().accept(port).await.unwrap();
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
            _tunnel: tunnel,
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
            let server = echo_server(&client_key.public_key(), 2222).await;

            let transport = WireGuardTransport::start(
                CLIENT_ADDRESS.parse().unwrap(),
                1420,
                client_key,
                &server.public_key,
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
    fn a_server_that_does_not_know_this_device_is_reported_as_no_handshake() {
        runtime().block_on(async {
            // The server expects a different device's key.
            let stranger = DeviceKey::generate().public_key();
            let server = echo_server(&stranger, 2222).await;

            let transport = WireGuardTransport::start(
                CLIENT_ADDRESS.parse().unwrap(),
                1420,
                DeviceKey::generate(),
                &server.public_key,
                localhost(server.udp_port),
            )
            .await
            .unwrap();
            tokio::time::pause();
            let target: SocketAddr = format!("{SERVER_ADDRESS}:2222").parse().unwrap();
            let error = transport.connect(target).await.err().unwrap();
            assert!(
                matches!(error, TransportError::NoHandshake { endpoint } if endpoint == localhost(server.udp_port)),
                "{error}"
            );
        });
    }

    #[test]
    fn a_silent_address_behind_a_working_tunnel_is_a_timeout_not_a_handshake_failure() {
        runtime().block_on(async {
            let client_key = DeviceKey::generate();
            let server = echo_server(&client_key.public_key(), 2222).await;
            let transport = WireGuardTransport::start(
                CLIENT_ADDRESS.parse().unwrap(),
                1420,
                client_key,
                &server.public_key,
                localhost(server.udp_port),
            )
            .await
            .unwrap();
            // A first connection proves the handshake has completed.
            let reachable: SocketAddr = format!("{SERVER_ADDRESS}:2222").parse().unwrap();
            let _stream = transport.connect(reachable).await.unwrap();

            tokio::time::pause();
            // Nothing in the server's network has this address.
            let silent: SocketAddr = "10.0.0.9:2222".parse().unwrap();
            let error = transport.connect(silent).await.err().unwrap();
            assert!(
                matches!(error, TransportError::Timeout { target } if target == silent),
                "{error}"
            );
        });
    }

    #[test]
    fn endpoints_prefer_ipv4_even_when_ipv6_comes_first() {
        let v6: SocketAddr = "[64:ff9b::c000:20a]:51820".parse().unwrap();
        let v4: SocketAddr = "192.0.2.10:51820".parse().unwrap();
        assert_eq!(pick_endpoint([v6, v4]), Some(v4));
        assert_eq!(pick_endpoint([v4, v6]), Some(v4));
    }

    #[test]
    fn endpoints_fall_back_to_ipv6_when_there_is_no_ipv4() {
        let first: SocketAddr = "[2001:db8::1]:51820".parse().unwrap();
        let second: SocketAddr = "[2001:db8::2]:51820".parse().unwrap();
        assert_eq!(pick_endpoint([first, second]), Some(first));
        assert_eq!(pick_endpoint([]), None);
    }

    #[test]
    fn this_devices_own_key_as_the_server_key_is_refused_with_advice() {
        runtime().block_on(async {
            let key = DeviceKey::generate();
            let settings = WireGuard {
                endpoint: "vpn.example.net:51820".parse().unwrap(),
                server_public_key: key.public_key(),
                address: Some("10.0.0.2/32".parse().unwrap()),
                mtu: None,
            };
            let error = WireGuardTransport::open("home", &settings, key)
                .await
                .err()
                .unwrap();
            let message = error.to_string();
            assert!(matches!(error, TransportError::OwnKey { .. }), "{message}");
            assert!(message.contains("gitaway profile set home"), "{message}");
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
