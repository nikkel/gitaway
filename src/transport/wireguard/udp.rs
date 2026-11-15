//! The tunnel's UDP socket: gotatun's own, made to survive "connection
//! reset" errors, and counting the datagrams that arrive.
//!
//! On Windows, when a UDP packet arrives at a port where nothing listens,
//! the *sender's* socket reports it on its next receive, as a "connection
//! reset" error (`WSAECONNRESET`):
//!
//! ```text
//!  our socket ──packet──▶ port nobody listens on any more
//!             ◀─ICMP "port unreachable"─
//!  next recv_from() ──▶ Err(ConnectionReset)   (Windows only)
//! ```
//!
//! An ICMP "time exceeded" (a packet that ran out of hops on the way) is
//! reported the same way, as "network reset" (`WSAENETRESET`). std gives
//! that one no `io::ErrorKind`, so it's recognised by its code.
//!
//! For a socket that talks to whoever sends it packets, these errors mean
//! nothing: the next packet may come from somewhere else entirely. But
//! gotatun stops receiving for good on any error, so one stray packet to a
//! closed port (a peer that has just exited, a VPN server restarting) would
//! leave the tunnel deaf. [`SkipResets`] drops both errors and keeps
//! receiving. Other systems don't report them on unconnected sockets, so it
//! changes nothing there.
//!
//! Both of gotatun's receive methods are wrapped. gotatun calls the batched
//! one, `recv_many_from`, and on Linux only the socket's own version of it
//! splits the datagrams the kernel merges (UDP GRO), so it must be passed
//! through rather than left to the trait's one-packet default.
//!
//! Reported upstream as <https://github.com/mullvad/gotatun/issues/226>.
//! Once gotatun handles it, [`SkipResets`] can go.
//!
//! [`Received`] counts every datagram the socket receives, so a timed-out
//! connection can tell whether the VPN server was still answering. gotatun's
//! own byte count can't: it leaves out keepalives, which are often all a
//! server sends.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gotatun::packet::{Packet, PacketBufPool};
use gotatun::udp::socket::{UdpSocket, UdpSocketFactory};
use gotatun::udp::{UdpRecv, UdpTransportFactory, UdpTransportFactoryParams};

/// Windows' `WSAENETRESET`: an ICMP "time exceeded" for a packet this socket
/// sent. std has no `ErrorKind` for it.
const WSAENETRESET: i32 = 10052;

/// Whether `error` is Windows reporting one ICMP message about a packet this
/// socket sent, rather than a problem with the socket: "connection reset"
/// (port unreachable) or "network reset" (time exceeded).
fn is_icmp_report(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::ConnectionReset
        || (cfg!(windows) && error.raw_os_error() == Some(WSAENETRESET))
}

/// How many datagrams the tunnel's socket has received, from anywhere.
///
/// Clones share one count. In practice everything comes from the VPN
/// server: nothing else knows the socket's random port. A stray datagram
/// would only change which of two timeout messages is shown.
#[derive(Debug, Clone, Default)]
pub struct Received(Arc<AtomicU64>);

impl Received {
    /// The number so far.
    pub fn count(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    fn add(&self, datagrams: usize) {
        self.0.fetch_add(datagrams as u64, Ordering::Relaxed);
    }
}

/// gotatun's ordinary UDP sockets, with [`SkipResets`] and [`Counted`] on
/// the receive side.
#[derive(Debug, Default)]
pub struct UdpFactory {
    sockets: UdpSocketFactory,
    received: Received,
}

impl UdpFactory {
    /// The count of datagrams received by the sockets this factory makes.
    pub fn received(&self) -> Received {
        self.received.clone()
    }
}

impl UdpTransportFactory for UdpFactory {
    type Send = UdpSocket;
    type Recv = SkipResets<Counted<UdpSocket>>;

    async fn bind(
        &mut self,
        params: &UdpTransportFactoryParams,
    ) -> io::Result<(Self::Send, Self::Recv)> {
        let (send, recv) = self.sockets.bind(params).await?;
        Ok((send, SkipResets(Counted(recv, self.received.clone()))))
    }
}

/// A receiving socket that ignores Windows' ICMP reports: "connection reset"
/// and "network reset".
#[derive(Debug)]
pub struct SkipResets<R>(R);

impl<R: UdpRecv> UdpRecv for SkipResets<R> {
    type RecvManyBuf = R::RecvManyBuf;

    async fn recv_from(&mut self, pool: &mut PacketBufPool) -> io::Result<(Packet, SocketAddr)> {
        loop {
            match self.0.recv_from(pool).await {
                Err(error) if is_icmp_report(&error) => continue,
                other => return other,
            }
        }
    }

    // gotatun receives through this method. It must reach the socket's own
    // version: on Linux that's the one that splits datagrams merged by UDP
    // GRO, which the socket has turned on.
    async fn recv_many_from(
        &mut self,
        recv_buf: &mut Self::RecvManyBuf,
        pool: &mut PacketBufPool,
        packets: &mut Vec<(Packet, SocketAddr)>,
    ) -> io::Result<()> {
        loop {
            match self.0.recv_many_from(recv_buf, pool, packets).await {
                Err(error) if is_icmp_report(&error) => continue,
                other => return other,
            }
        }
    }

    fn enable_udp_gro(&self) -> io::Result<()> {
        self.0.enable_udp_gro()
    }
}

/// A receiving socket that adds each datagram it receives to a [`Received`].
#[derive(Debug)]
pub struct Counted<R>(R, Received);

impl<R: UdpRecv> UdpRecv for Counted<R> {
    type RecvManyBuf = R::RecvManyBuf;

    async fn recv_from(&mut self, pool: &mut PacketBufPool) -> io::Result<(Packet, SocketAddr)> {
        let received = self.0.recv_from(pool).await?;
        self.1.add(1);
        Ok(received)
    }

    // Passed through for the same reason as in `SkipResets`.
    async fn recv_many_from(
        &mut self,
        recv_buf: &mut Self::RecvManyBuf,
        pool: &mut PacketBufPool,
        packets: &mut Vec<(Packet, SocketAddr)>,
    ) -> io::Result<()> {
        let before = packets.len();
        self.0.recv_many_from(recv_buf, pool, packets).await?;
        self.1.add(packets.len().saturating_sub(before));
        Ok(())
    }

    fn enable_udp_gro(&self) -> io::Result<()> {
        self.0.enable_udp_gro()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A receiving socket that gives out prepared results, in order. A
    /// batched receive gives two packets at once, as a socket with UDP GRO
    /// does when the kernel has merged datagrams.
    struct Scripted(VecDeque<io::Result<()>>);

    impl UdpRecv for Scripted {
        type RecvManyBuf = ();

        async fn recv_from(
            &mut self,
            pool: &mut PacketBufPool,
        ) -> io::Result<(Packet, SocketAddr)> {
            let next = self.0.pop_front().expect("no more scripted results");
            next.map(|()| (pool.get(), "192.0.2.10:51820".parse().unwrap()))
        }

        async fn recv_many_from(
            &mut self,
            _recv_buf: &mut (),
            pool: &mut PacketBufPool,
            packets: &mut Vec<(Packet, SocketAddr)>,
        ) -> io::Result<()> {
            let next = self.0.pop_front().expect("no more scripted results");
            next.map(|()| {
                for port in [1, 2] {
                    packets.push((pool.get(), SocketAddr::from(([192, 0, 2, 10], port))));
                }
            })
        }
    }

    fn receive_many(script: Vec<io::Result<()>>) -> (io::Result<Vec<SocketAddr>>, usize) {
        let mut socket = SkipResets(Scripted(script.into()));
        let mut pool = PacketBufPool::new(2);
        let mut packets = Vec::new();
        let result = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(socket.recv_many_from(&mut Default::default(), &mut pool, &mut packets))
            .map(|()| packets.iter().map(|(_, from)| *from).collect());
        (result, socket.0.0.len())
    }

    fn receive(script: Vec<io::Result<()>>) -> (io::Result<SocketAddr>, usize) {
        let mut socket = SkipResets(Scripted(script.into()));
        let mut pool = PacketBufPool::new(1);
        let result = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(socket.recv_from(&mut pool))
            .map(|(_, from)| from);
        (result, socket.0.0.len())
    }

    fn reset() -> io::Result<()> {
        Err(io::ErrorKind::ConnectionReset.into())
    }

    #[test]
    fn connection_resets_are_skipped_until_a_packet_arrives() {
        let (result, left) = receive(vec![reset(), reset(), Ok(()), reset()]);
        assert_eq!(result.unwrap(), "192.0.2.10:51820".parse().unwrap());
        assert_eq!(left, 1, "it read past the packet");
    }

    /// gotatun receives in batches through `recv_many_from`; on Linux that
    /// is what splits datagrams the kernel merged (UDP GRO, which gotatun
    /// turns on). Falling back to one `recv_from` would hand gotatun the
    /// merged bytes as one packet, which it can't decrypt.
    #[test]
    fn batched_receives_are_passed_through_and_skip_resets() {
        let (result, left) = receive_many(vec![reset(), Ok(()), Ok(())]);
        let from: Vec<u16> = result.unwrap().iter().map(SocketAddr::port).collect();
        assert_eq!(from, [1, 2]);
        assert_eq!(left, 1, "it read past the batch");

        let (result, _) = receive_many(vec![Err(io::ErrorKind::PermissionDenied.into())]);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    /// Every datagram counts, one by one or in a batch, as the tunnel's
    /// socket is built: resets skipped, then counted. Errors don't count.
    #[test]
    fn every_datagram_received_is_counted_and_errors_are_not() {
        let received = Received::default();
        let script = vec![reset(), Ok(()), reset(), Ok(())];
        let mut socket = SkipResets(Counted(Scripted(script.into()), received.clone()));
        let mut pool = PacketBufPool::new(2);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        runtime.block_on(socket.recv_from(&mut pool)).unwrap();
        assert_eq!(received.count(), 1);
        let mut packets = Vec::new();
        runtime
            .block_on(socket.recv_many_from(&mut (), &mut pool, &mut packets))
            .unwrap();
        assert_eq!(received.count(), 3, "a batch of two counts as two");
    }

    /// Windows reports an ICMP "time exceeded" (a datagram that ran out of
    /// hops) on the next receive as `WSAENETRESET`, 10052. std gives it no
    /// `ErrorKind`, so it must be recognised by its code, on both receive
    /// methods.
    #[cfg(windows)]
    #[test]
    fn network_resets_are_skipped_too() {
        let net_reset = || Err(io::Error::from_raw_os_error(10052));

        let (result, left) = receive(vec![net_reset(), reset(), Ok(())]);
        assert_eq!(result.unwrap(), "192.0.2.10:51820".parse().unwrap());
        assert_eq!(left, 0);

        let (result, left) = receive_many(vec![net_reset(), Ok(())]);
        assert_eq!(result.unwrap().len(), 2);
        assert_eq!(left, 0);
    }

    #[test]
    fn other_errors_still_end_the_receive() {
        let (result, _) = receive(vec![reset(), Err(io::ErrorKind::PermissionDenied.into())]);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }
}
