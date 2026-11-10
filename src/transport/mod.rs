//! Transports: how gitaway reaches home from elsewhere.
//!
//! A transport does one thing: open a TCP connection to an address on the
//! home network. Everything above it (the local proxy git talks to, profile
//! choice, the home check) only uses [`Transport::connect`], so a new kind
//! of VPN never touches that code.
//!
//! ```text
//!  proxy ──connect(10.0.0.1:2222)──▶ dyn Transport ──▶ stream to the server
//!                                       ├─ WireGuard  (wireguard/)
//!                                       └─ … a new transport is a new folder
//! ```
//!
//! Dropping a transport closes the tunnel and stops its background tasks.
//!
//! [`client_config`] reads the config file a VPN server gives a device, so
//! `gitaway setup` can make a profile from it.
//!
//! # Extending
//!
//! A new transport is a new [`crate::config::profile::Transport`] variant
//! (its settings), a new folder here implementing [`Transport`], an arm in
//! [`open`], and a reader for its client config in
//! [`client_config::FORMATS`]. A VPN that carries IP packets can reuse [`netstack`] for TCP,
//! as WireGuard does. See `docs/adding-a-transport.md`.

pub mod client_config;
pub mod netstack;
pub mod wireguard;

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::config::profile::{self, Profile};
use crate::keys::store::DeviceSecrets;

/// How long to wait for a connection through the tunnel, including the VPN
/// handshake the first connection triggers.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A byte stream to a server on the home network.
pub trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Stream for T {}

/// The future returned by [`Transport::connect`].
pub type ConnectFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn Stream>, TransportError>> + Send + 'a>>;

/// An open tunnel that can make TCP connections to the home network.
pub trait Transport: Send + Sync {
    /// Opens a TCP connection to `target`, an address as seen from inside
    /// the home network (the right-hand side of a `[hosts]` entry).
    fn connect(&self, target: SocketAddr) -> ConnectFuture<'_>;
}

/// The tunnel couldn't be opened, or a connection through it failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransportError {
    /// The profile has no tunnel address yet.
    #[error(
        "profile `{profile}` has no tunnel address. Once the server has this device \
         (see `gitaway peer {profile}`), set it with \
         `gitaway profile set {profile} transport.wireguard.address <address>/32`"
    )]
    NoAddress {
        /// The profile's name.
        profile: String,
    },
    /// The profile's server key is this device's own public key, a common
    /// mix-up when copying keys from a client config.
    #[error(
        "profile `{profile}`'s server public key is this device's own public key. Set the \
         server's key, the PublicKey under [Peer] in a client config: \
         `gitaway profile set {profile} transport.wireguard.server_public_key <key>`"
    )]
    OwnKey {
        /// The profile's name.
        profile: String,
    },
    /// The VPN server's address couldn't be looked up.
    #[error("couldn't find the VPN server {endpoint}: {reason}")]
    Endpoint {
        /// The endpoint as configured.
        endpoint: String,
        /// Why the lookup failed.
        reason: String,
    },
    /// The tunnel couldn't be started on this machine.
    #[error("couldn't start the tunnel: {0}")]
    Setup(String),
    /// The VPN server never completed a handshake, so nothing could cross
    /// the tunnel.
    #[error(
        "no answer from the VPN server at {endpoint} within {} seconds. Check that the \
         profile's server public key is right, that the server has this device's public key \
         (`gitaway peer`), and that UDP port {} reaches it from here",
        CONNECT_TIMEOUT.as_secs(),
        endpoint.port()
    )]
    NoHandshake {
        /// The server's address, as looked up.
        endpoint: SocketAddr,
    },
    /// The tunnel is up, but nothing answered at the address inside it.
    #[error(
        "the VPN server answered, but {target} didn't within {} seconds. Check that the \
         server can reach {target} (the right-hand side of the profile's `[hosts]` entry) \
         and allows this device to reach it",
        CONNECT_TIMEOUT.as_secs()
    )]
    Timeout {
        /// The address inside the home network.
        target: SocketAddr,
    },
    /// The connection through the tunnel failed.
    #[error("couldn't connect to {target} through the tunnel: {source}")]
    Connect {
        /// The address inside the home network.
        target: SocketAddr,
        /// What went wrong.
        source: io::Error,
    },
}

/// Opens the profile's tunnel with this device's secrets.
pub async fn open(
    name: &str,
    profile: &Profile,
    secrets: DeviceSecrets,
) -> Result<Box<dyn Transport>, TransportError> {
    match &profile.transport {
        profile::Transport::WireGuard(settings) => Ok(Box::new(
            wireguard::WireGuardTransport::open(name, settings, secrets).await?,
        )),
    }
}
