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
    type RecvManyBuf = ();

    async fn recv_from(&mut self, pool: &mut PacketBufPool) -> io::Result<(Packet, SocketAddr)> {
        loop {
            match self.0.recv_from(pool).await {
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

    /// A receiving socket that gives out prepared results, in order.
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

    #[test]
    fn other_errors_still_end_the_receive() {
        let (result, _) = receive(vec![reset(), Err(io::ErrorKind::PermissionDenied.into())]);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }
}
