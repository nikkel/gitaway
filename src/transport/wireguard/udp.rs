//! The tunnel's UDP socket: gotatun's own, made to survive "connection
//! reset" errors.
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
//! For a socket that talks to whoever sends it packets, that error means
//! nothing: the next packet may come from somewhere else entirely. But
//! gotatun stops receiving for good on any error, so one stray packet to a
//! closed port (a peer that has just exited, a VPN server restarting) would
//! leave the tunnel deaf. [`SkipResets`] drops those errors and keeps
//! receiving. Other systems don't report them on unconnected sockets, so it
//! changes nothing there.
//!
//! Both of gotatun's receive methods are wrapped. gotatun calls the batched
//! one, `recv_many_from`, and on Linux only the socket's own version of it
//! splits the datagrams the kernel merges (UDP GRO), so it must be passed
//! through rather than left to the trait's one-packet default.
//!
//! Reported upstream as <https://github.com/mullvad/gotatun/issues/226>.
//! Once gotatun handles it, this module can go: use `with_default_udp` again.

use std::io;
use std::net::SocketAddr;

use gotatun::packet::{Packet, PacketBufPool};
use gotatun::udp::socket::{UdpSocket, UdpSocketFactory};
use gotatun::udp::{UdpRecv, UdpTransportFactory, UdpTransportFactoryParams};

/// gotatun's ordinary UDP sockets, with [`SkipResets`] on the receive side.
#[derive(Debug, Default)]
pub struct UdpFactory(UdpSocketFactory);

impl UdpTransportFactory for UdpFactory {
    type Send = UdpSocket;
    type Recv = SkipResets<UdpSocket>;

    async fn bind(
        &mut self,
        params: &UdpTransportFactoryParams,
    ) -> io::Result<(Self::Send, Self::Recv)> {
        let (send, recv) = self.0.bind(params).await?;
        Ok((send, SkipResets(recv)))
    }
}

/// A receiving socket that ignores "connection reset" errors.
#[derive(Debug)]
pub struct SkipResets<R>(R);

impl<R: UdpRecv> UdpRecv for SkipResets<R> {
    type RecvManyBuf = R::RecvManyBuf;

    async fn recv_from(&mut self, pool: &mut PacketBufPool) -> io::Result<(Packet, SocketAddr)> {
        loop {
            match self.0.recv_from(pool).await {
                Err(error) if error.kind() == io::ErrorKind::ConnectionReset => continue,
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
                Err(error) if error.kind() == io::ErrorKind::ConnectionReset => continue,
                other => return other,
            }
        }
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

    #[test]
    fn other_errors_still_end_the_receive() {
        let (result, _) = receive(vec![reset(), Err(io::ErrorKind::PermissionDenied.into())]);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }
}
