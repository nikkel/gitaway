//! A TCP/IP stack in userspace, so a VPN can carry TCP without a network
//! adapter or root.
//!
//! A VPN tunnel carries raw IP packets. To open a TCP connection through it,
//! something has to build those packets: normally the operating system's
//! network stack, which would need a virtual adapter and root. Here it's
//! [`smoltcp`], running inside gitaway.
//!
//! ```text
//!  TcpStream (AsyncRead + AsyncWrite)        what callers use
//!       │  ▲
//!       ▼  │  smoltcp sockets, woken by smoltcp's wakers
//!  ┌──────────────────────────────┐
//!  │ NetStack task: smoltcp       │  polls the interface whenever a packet
//!  │ interface + socket set       │  arrives, a stream reads or writes, or a
//!  └──────────────────────────────┘  TCP timer is due
//!       │  ▲
//!       ▼  │  raw IP packets
//!  PacketPipe  ─────────────────────▶  the VPN (WireGuard, or another)
//! ```
//!
//! Nothing here knows about WireGuard: any transport that moves IP packets
//! can reuse it.
//!
//! Streams lock the shared state only for short, non-blocking calls into
//! smoltcp, and never across an `.await`.

use std::collections::VecDeque;
use std::future::poll_fn;
use std::hash::{BuildHasher, RandomState};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::Duration;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, DeviceCapabilities, Medium};
use smoltcp::socket::tcp;
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;

/// Each TCP socket's receive and send buffer. Large enough for good
/// throughput on a long-distance link (smoltcp uses window scaling).
const SOCKET_BUFFER: usize = 512 * 1024;

/// How many packets may queue between the stack and the VPN in each direction.
const PACKET_QUEUE: usize = 1024;

/// How long a TCP connection may go without any answer before it's dropped.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(60);

/// The first local port used for outgoing connections (the IANA ephemeral range).
const EPHEMERAL_PORTS: std::ops::RangeInclusive<u16> = 49152..=65535;

/// The packet side of a [`NetStack`], for the VPN to drive.
pub struct PacketPipe {
    /// IP packets that arrived through the tunnel, for the stack.
    pub inbound: mpsc::Sender<Vec<u8>>,
    /// IP packets the stack wants sent through the tunnel.
    pub outbound: mpsc::Receiver<Vec<u8>>,
}

/// A userspace TCP/IP stack with one address. Dropping it stops the stack;
/// streams still open start failing.
pub struct NetStack {
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

/// A TCP connection through a [`NetStack`].
pub struct TcpStream {
    shared: Arc<Shared>,
    handle: SocketHandle,
}

struct Shared {
    state: Mutex<State>,
    /// Wakes the stack task to poll the interface (data written, buffer
    /// space freed, a socket added or closed).
    poll_needed: Notify,
}

struct State {
    iface: Interface,
    sockets: SocketSet<'static>,
    device: Queues,
    address: IpAddr,
    next_port: u16,
    /// Sockets whose stream was dropped, removed once they've finished closing.
    closing: Vec<SocketHandle>,
    started: std::time::Instant,
}

/// The smoltcp device: packets waiting to go in or out.
struct Queues {
    rx: VecDeque<Vec<u8>>,
    tx: VecDeque<Vec<u8>>,
    mtu: usize,
}

impl NetStack {
    /// Starts a stack with `address` as its only IP address. `mtu` is the
    /// largest IP packet the VPN can carry. Must be called inside a tokio
    /// runtime.
    pub fn new(address: IpAddr, mtu: usize) -> (Self, PacketPipe) {
        let mut device = Queues {
            rx: VecDeque::new(),
            tx: VecDeque::new(),
            mtu,
        };
        let random = RandomState::new().hash_one(address);
        let mut config = Config::new(HardwareAddress::Ip);
        config.random_seed = random;
        let started = std::time::Instant::now();
        let mut iface = Interface::new(config, &mut device, SmolInstant::from_micros(0));
        let full_prefix = if address.is_ipv4() { 32 } else { 128 };
        iface.update_ip_addrs(|addrs| {
            addrs
                .push(IpCidr::new(IpAddress::from(address), full_prefix))
                .expect("a new interface has room for one address");
        });
        // Everything that isn't this address goes into the tunnel. With an
        // IP-only medium the gateway is never contacted, but smoltcp needs a
        // route to send anything.
        match address {
            IpAddr::V4(own) => {
                iface.routes_mut().add_default_ipv4_route(own).ok();
            }
            IpAddr::V6(own) => {
                iface.routes_mut().add_default_ipv6_route(own).ok();
            }
        }

        let ports = EPHEMERAL_PORTS.end() - EPHEMERAL_PORTS.start();
        let state = State {
            iface,
            sockets: SocketSet::new(Vec::new()),
            device,
            address,
            next_port: EPHEMERAL_PORTS.start() + (random % u64::from(ports)) as u16,
            closing: Vec::new(),
            started,
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            poll_needed: Notify::new(),
        });
        let (inbound_tx, inbound_rx) = mpsc::channel(PACKET_QUEUE);
        let (outbound_tx, outbound_rx) = mpsc::channel(PACKET_QUEUE);
        let task = tokio::spawn(run(shared.clone(), inbound_rx, outbound_tx));
        let pipe = PacketPipe {
            inbound: inbound_tx,
            outbound: outbound_rx,
        };
        (Self { shared, task }, pipe)
    }

    /// Opens a TCP connection to `target`, waiting until it's established.
    ///
    /// Fails with `ConnectionRefused` if the target refuses or resets, and
    /// `Unsupported` if `target` is IPv6 but the stack's address is IPv4 (or
    /// the other way round). The caller sets the overall time limit.
    pub async fn connect(&self, target: SocketAddr) -> io::Result<TcpStream> {
        let handle = {
            let mut state = lock(&self.shared);
            if state.address.is_ipv4() != target.is_ipv4() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "the tunnel address {} can't reach {}: they're different IP versions",
                        state.address, target
                    ),
                ));
            }
            let port = state.take_port();
            let mut socket = new_socket();
            let State { iface, .. } = &mut *state;
            socket
                .connect(iface.context(), target, port)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
            state.sockets.add(socket)
        };
        let stream = TcpStream {
            shared: self.shared.clone(),
            handle,
        };
        self.shared.poll_needed.notify_one();

        poll_fn(|cx| {
            let mut state = lock(&self.shared);
            let socket = state.sockets.get_mut::<tcp::Socket>(handle);
            match socket.state() {
                tcp::State::Established => Poll::Ready(Ok(())),
                tcp::State::SynSent | tcp::State::SynReceived => {
                    socket.register_send_waker(cx.waker());
                    Poll::Pending
                }
                _ => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!("{target} refused the connection"),
                ))),
            }
        })
        .await?;
        Ok(stream)
    }

    /// Waits for one incoming TCP connection on `port`. A WireGuard server's
    /// side of the tunnel uses this (in tests, and in the example
    /// environment's tooling); gitaway itself only connects out.
    pub async fn accept(&self, port: u16) -> io::Result<TcpStream> {
        let handle = {
            let mut state = lock(&self.shared);
            let mut socket = new_socket();
            socket
                .listen(port)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
            state.sockets.add(socket)
        };
        let stream = TcpStream {
            shared: self.shared.clone(),
            handle,
        };
        poll_fn(|cx| {
            let mut state = lock(&self.shared);
            let socket = state.sockets.get_mut::<tcp::Socket>(handle);
            match socket.state() {
                tcp::State::Listen | tcp::State::SynReceived => {
                    socket.register_recv_waker(cx.waker());
                    socket.register_send_waker(cx.waker());
                    Poll::Pending
                }
                _ => Poll::Ready(()),
            }
        })
        .await;
        Ok(stream)
    }
}

impl Drop for NetStack {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl State {
    fn take_port(&mut self) -> u16 {
        let port = self.next_port;
        self.next_port = if port == *EPHEMERAL_PORTS.end() {
            *EPHEMERAL_PORTS.start()
        } else {
            port + 1
        };
        port
    }

    fn now(&self) -> SmolInstant {
        SmolInstant::from_micros(self.started.elapsed().as_micros() as i64)
    }

    /// Polls the interface: takes in received packets, runs TCP, and queues
    /// packets to send. Returns how long until it needs polling again.
    fn poll(&mut self) -> Option<Duration> {
        let now = self.now();
        let State {
            iface,
            sockets,
            device,
            ..
        } = self;
        iface.poll(now, device, sockets);
        self.remove_closed();
        self.iface
            .poll_delay(now, &self.sockets)
            .map(|delay| Duration::from_micros(delay.total_micros()))
    }

    fn remove_closed(&mut self) {
        let sockets = &mut self.sockets;
        self.closing.retain(|&handle| {
            let state = sockets.get::<tcp::Socket>(handle).state();
            let done = matches!(state, tcp::State::Closed | tcp::State::TimeWait);
            if done {
                sockets.remove(handle);
            }
            !done
        });
    }
}

fn new_socket() -> tcp::Socket<'static> {
    let mut socket = tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0; SOCKET_BUFFER]),
        tcp::SocketBuffer::new(vec![0; SOCKET_BUFFER]),
    );
    socket.set_timeout(Some(SOCKET_TIMEOUT.into()));
    socket.set_nagle_enabled(false);
    socket
}

/// Locks the shared state. A panic while it was held can't leave smoltcp's
/// state half-updated in a way that matters more than failing connections,
/// so a poisoned lock is still used.
fn lock(shared: &Shared) -> MutexGuard<'_, State> {
    shared
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The stack task: poll, send what's queued, then sleep until there's
/// something to do.
async fn run(
    shared: Arc<Shared>,
    mut inbound: mpsc::Receiver<Vec<u8>>,
    outbound: mpsc::Sender<Vec<u8>>,
) {
    loop {
        let (delay, to_send) = {
            let mut state = lock(&shared);
            while let Ok(packet) = inbound.try_recv() {
                state.device.rx.push_back(packet);
            }
            let delay = state.poll();
            (delay, std::mem::take(&mut state.device.tx))
        };
        for packet in to_send {
            if outbound.send(packet).await.is_err() {
                return; // the VPN side is gone
            }
        }
        let sleep = delay.unwrap_or(Duration::from_secs(3600));
        tokio::select! {
            packet = inbound.recv() => match packet {
                Some(packet) => lock(&shared).device.rx.push_back(packet),
                None => return,
            },
            () = shared.poll_needed.notified() => {}
            () = tokio::time::sleep(sleep) => {}
        }
    }
}

impl AsyncRead for TcpStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut state = lock(&self.shared);
        let socket = state.sockets.get_mut::<tcp::Socket>(self.handle);
        match socket.recv_slice(buf.initialize_unfilled()) {
            Ok(0) => {
                socket.register_recv_waker(cx.waker());
                Poll::Pending
            }
            Ok(n) => {
                buf.advance(n);
                drop(state);
                // Freed buffer space may let the window open: tell the peer.
                self.shared.poll_needed.notify_one();
                Poll::Ready(Ok(()))
            }
            Err(tcp::RecvError::Finished) => Poll::Ready(Ok(())), // end of stream
            Err(tcp::RecvError::InvalidState) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "the connection through the tunnel was reset",
            ))),
        }
    }
}

impl AsyncWrite for TcpStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut state = lock(&self.shared);
        let socket = state.sockets.get_mut::<tcp::Socket>(self.handle);
        if !socket.may_send() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the connection through the tunnel is closed",
            )));
        }
        match socket.send_slice(data) {
            Ok(0) => {
                socket.register_send_waker(cx.waker());
                Poll::Pending
            }
            Ok(n) => {
                drop(state);
                self.shared.poll_needed.notify_one();
                Poll::Ready(Ok(n))
            }
            Err(tcp::SendError::InvalidState) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the connection through the tunnel is closed",
            ))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Written data is already queued in smoltcp, which sends it as soon
        // as the window allows.
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        lock(&self.shared)
            .sockets
            .get_mut::<tcp::Socket>(self.handle)
            .close();
        self.shared.poll_needed.notify_one();
        Poll::Ready(Ok(()))
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        let mut state = lock(&self.shared);
        state.sockets.get_mut::<tcp::Socket>(self.handle).close();
        state.closing.push(self.handle);
        drop(state);
        self.shared.poll_needed.notify_one();
    }
}

impl phy::Device for Queues {
    type RxToken<'a> = RxToken;
    type TxToken<'a> = TxToken<'a>;

    fn receive(&mut self, _now: SmolInstant) -> Option<(RxToken, TxToken<'_>)> {
        let packet = self.rx.pop_front()?;
        Some((RxToken(packet), TxToken(&mut self.tx)))
    }

    fn transmit(&mut self, _now: SmolInstant) -> Option<TxToken<'_>> {
        Some(TxToken(&mut self.tx))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip;
        caps.max_transmission_unit = self.mtu;
        caps
    }
}

struct RxToken(Vec<u8>);

impl phy::RxToken for RxToken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

struct TxToken<'a>(&'a mut VecDeque<Vec<u8>>);

impl phy::TxToken for TxToken<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut packet = vec![0; len];
        let result = f(&mut packet);
        self.0.push_back(packet);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Connects two stacks back to back, as if through a perfect tunnel.
    fn wire(a: PacketPipe, b: PacketPipe) {
        let PacketPipe {
            inbound: a_in,
            outbound: mut a_out,
        } = a;
        let PacketPipe {
            inbound: b_in,
            outbound: mut b_out,
        } = b;
        tokio::spawn(async move {
            while let Some(packet) = a_out.recv().await {
                if b_in.send(packet).await.is_err() {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            while let Some(packet) = b_out.recv().await {
                if a_in.send(packet).await.is_err() {
                    break;
                }
            }
        });
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn data_flows_both_ways_and_close_is_seen() {
        runtime().block_on(async {
            let (client, client_pipe) = NetStack::new("10.0.0.2".parse().unwrap(), 1420);
            let (server, server_pipe) = NetStack::new("10.0.0.1".parse().unwrap(), 1420);
            wire(client_pipe, server_pipe);

            let accept = tokio::spawn(async move {
                let mut stream = server.accept(22).await.unwrap();
                let mut received = Vec::new();
                stream.read_to_end(&mut received).await.unwrap();
                stream.write_all(&received).await.unwrap();
                stream.shutdown().await.unwrap();
                // Keep the server's stack alive until the client has read.
                tokio::time::sleep(Duration::from_secs(5)).await;
                drop(server);
            });

            let mut stream = client
                .connect("10.0.0.1:22".parse().unwrap())
                .await
                .unwrap();
            // More than one socket buffer, so flow control is exercised.
            let message: Vec<u8> = (0..SOCKET_BUFFER * 3).map(|i| (i % 251) as u8).collect();
            stream.write_all(&message).await.unwrap();
            stream.shutdown().await.unwrap();
            let mut echoed = Vec::new();
            stream.read_to_end(&mut echoed).await.unwrap();
            assert!(echoed == message, "echoed data differs");
            accept.abort();
        });
    }

    #[test]
    fn a_closed_port_refuses() {
        runtime().block_on(async {
            let (client, client_pipe) = NetStack::new("10.0.0.2".parse().unwrap(), 1420);
            let (_server, server_pipe) = NetStack::new("10.0.0.1".parse().unwrap(), 1420);
            wire(client_pipe, server_pipe);
            let error = client
                .connect("10.0.0.1:23".parse().unwrap())
                .await
                .err()
                .expect("nothing listens on port 23");
            assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
        });
    }

    #[test]
    fn the_wrong_ip_version_is_refused_at_once() {
        runtime().block_on(async {
            let (client, _pipe) = NetStack::new("10.0.0.2".parse().unwrap(), 1420);
            let error = client
                .connect("[fd00::1]:22".parse().unwrap())
                .await
                .err()
                .expect("an IPv4 stack can't reach IPv6");
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        });
    }
}
