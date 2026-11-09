//! The SOCKS5 protocol (RFC 1928), with username and password
//! authentication (RFC 1929): just the parts gitaway needs.
//!
//! ```text
//!  client                                   gitaway's proxy
//!  [5, n, methods…]            ───────────▶  must offer method 2 (username/password)
//!                              ◀───────────  [5, 2]
//!  [1, len, user, len, token]  ───────────▶  token checked in constant time
//!                              ◀───────────  [1, 0] ok  /  [1, 1] refused
//!  [5, 1 CONNECT, 0, type, address, port] ─▶ IPv4, IPv6 or a name
//!                              ◀───────────  [5, reply, 0, 1, 0.0.0.0, 0]
//!  … then bytes flow both ways
//! ```
//!
//! [`accept`] runs the server side; [`connect`] is the client side, used by
//! the `__proxy-connect` helper that ssh runs.

use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const VERSION: u8 = 5;
const AUTH_VERSION: u8 = 1;
const METHOD_PASSWORD: u8 = 2;
const NO_ACCEPTABLE_METHOD: u8 = 0xff;
const CMD_CONNECT: u8 = 1;
const ATYP_IPV4: u8 = 1;
const ATYP_DOMAIN: u8 = 3;
const ATYP_IPV6: u8 = 4;

/// The username clients send. Only the token (the password) matters.
pub const USERNAME: &str = "gitaway";

/// Where a client asked to connect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// An IP address or a name, without brackets.
    pub host: String,
    /// The port.
    pub port: u16,
}

/// The answer to a CONNECT request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Reply {
    /// Connected.
    Succeeded = 0,
    /// Something else went wrong.
    GeneralFailure = 1,
    /// The target couldn't be reached.
    HostUnreachable = 4,
    /// The target refused.
    ConnectionRefused = 5,
    /// No answer in time.
    TimedOut = 6,
    /// Only CONNECT is supported.
    CommandNotSupported = 7,
    /// The address type isn't IPv4, IPv6 or a name.
    AddressTypeNotSupported = 8,
}

impl Reply {
    /// A short description, for messages.
    pub fn describe(code: u8) -> &'static str {
        match code {
            0 => "connected",
            2 => "not allowed",
            3 => "network unreachable",
            4 => "host unreachable",
            5 => "connection refused",
            6 => "timed out",
            7 => "command not supported",
            8 => "address type not supported",
            _ => "the proxy couldn't connect",
        }
    }
}

/// A client that didn't follow the protocol, or wasn't allowed in.
#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    /// Reading or writing failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The client spoke something other than SOCKS5.
    #[error("not a SOCKS5 client")]
    NotSocks5,
    /// The client didn't offer username and password authentication.
    #[error("the client didn't offer password authentication")]
    NoPassword,
    /// The token was wrong.
    #[error("wrong proxy token")]
    WrongToken,
    /// Something other than CONNECT was asked for.
    #[error("only CONNECT is supported")]
    UnsupportedCommand,
    /// An address type gitaway doesn't know.
    #[error("unsupported address type")]
    UnsupportedAddress,
}

/// Runs the server side up to the CONNECT request. The caller then connects
/// and answers with [`reply`].
pub async fn accept<S>(stream: &mut S, token: &str) -> Result<Target, HandshakeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // Greeting: version and offered methods.
    if stream.read_u8().await? != VERSION {
        return Err(HandshakeError::NotSocks5);
    }
    let count = stream.read_u8().await?;
    let mut methods = vec![0; usize::from(count)];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&METHOD_PASSWORD) {
        stream.write_all(&[VERSION, NO_ACCEPTABLE_METHOD]).await?;
        return Err(HandshakeError::NoPassword);
    }
    stream.write_all(&[VERSION, METHOD_PASSWORD]).await?;

    // Username and password.
    if stream.read_u8().await? != AUTH_VERSION {
        return Err(HandshakeError::NotSocks5);
    }
    let _username = read_short_string(stream).await?;
    let password = read_short_string(stream).await?;
    if !constant_time_eq(&password, token.as_bytes()) {
        stream.write_all(&[AUTH_VERSION, 1]).await?;
        return Err(HandshakeError::WrongToken);
    }
    stream.write_all(&[AUTH_VERSION, 0]).await?;

    // The request.
    let mut header = [0; 4];
    stream.read_exact(&mut header).await?;
    let [version, command, _reserved, address_type] = header;
    if version != VERSION {
        return Err(HandshakeError::NotSocks5);
    }
    if command != CMD_CONNECT {
        reply(stream, Reply::CommandNotSupported).await?;
        return Err(HandshakeError::UnsupportedCommand);
    }
    let host = match address_type {
        ATYP_IPV4 => {
            let mut octets = [0; 4];
            stream.read_exact(&mut octets).await?;
            Ipv4Addr::from(octets).to_string()
        }
        ATYP_IPV6 => {
            let mut octets = [0; 16];
            stream.read_exact(&mut octets).await?;
            Ipv6Addr::from(octets).to_string()
        }
        ATYP_DOMAIN => {
            let bytes = read_short_string(stream).await?;
            match String::from_utf8(bytes) {
                Ok(name) => name,
                Err(_) => {
                    reply(stream, Reply::AddressTypeNotSupported).await?;
                    return Err(HandshakeError::UnsupportedAddress);
                }
            }
        }
        _ => {
            reply(stream, Reply::AddressTypeNotSupported).await?;
            return Err(HandshakeError::UnsupportedAddress);
        }
    };
    let port = stream.read_u16().await?;
    Ok(Target { host, port })
}

/// Answers a CONNECT request. gitaway doesn't report the bound address
/// (clients don't use it), so it's always `0.0.0.0:0`.
pub async fn reply<S: AsyncWrite + Unpin>(stream: &mut S, reply: Reply) -> io::Result<()> {
    stream
        .write_all(&[VERSION, reply as u8, 0, ATYP_IPV4, 0, 0, 0, 0, 0, 0])
        .await?;
    stream.flush().await
}

/// The client side, blocking: authenticates with `token` and asks for
/// `host:port`. On success the stream is ready to carry data.
pub fn connect(
    proxy: SocketAddr,
    token: &str,
    host: &str,
    port: u16,
) -> io::Result<std::net::TcpStream> {
    let protocol = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
    let mut stream = std::net::TcpStream::connect(proxy)?;
    stream.write_all(&[VERSION, 1, METHOD_PASSWORD])?;
    let mut answer = [0; 2];
    stream.read_exact(&mut answer)?;
    if answer != [VERSION, METHOD_PASSWORD] {
        return Err(protocol("the proxy refused password authentication"));
    }

    let mut auth = vec![AUTH_VERSION];
    push_short_string(&mut auth, USERNAME.as_bytes())?;
    push_short_string(&mut auth, token.as_bytes())?;
    stream.write_all(&auth)?;
    stream.read_exact(&mut answer)?;
    if answer != [AUTH_VERSION, 0] {
        return Err(protocol("the proxy refused the token"));
    }

    let mut request = vec![VERSION, CMD_CONNECT, 0];
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            request.push(ATYP_IPV4);
            request.extend(ip.octets());
        }
        Ok(IpAddr::V6(ip)) => {
            request.push(ATYP_IPV6);
            request.extend(ip.octets());
        }
        Err(_) => {
            request.push(ATYP_DOMAIN);
            push_short_string(&mut request, host.as_bytes())?;
        }
    }
    request.extend(port.to_be_bytes());
    stream.write_all(&request)?;

    let mut header = [0; 4];
    stream.read_exact(&mut header)?;
    if header[0] != VERSION {
        return Err(protocol("the proxy isn't a SOCKS5 server"));
    }
    if header[1] != Reply::Succeeded as u8 {
        return Err(io::Error::other(Reply::describe(header[1])));
    }
    // Skip the bound address, which gitaway doesn't use.
    let address_len = match header[3] {
        ATYP_IPV4 => 4,
        ATYP_IPV6 => 16,
        ATYP_DOMAIN => {
            let mut len = [0; 1];
            stream.read_exact(&mut len)?;
            usize::from(len[0])
        }
        _ => return Err(protocol("the proxy sent an unknown address type")),
    };
    let mut rest = vec![0; address_len + 2];
    stream.read_exact(&mut rest)?;
    Ok(stream)
}

async fn read_short_string<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<Vec<u8>> {
    let len = stream.read_u8().await?;
    let mut bytes = vec![0; usize::from(len)];
    stream.read_exact(&mut bytes).await?;
    Ok(bytes)
}

fn push_short_string(buf: &mut Vec<u8>, bytes: &[u8]) -> io::Result<()> {
    let len = u8::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "longer than 255 bytes"))?;
    buf.push(len);
    buf.extend_from_slice(bytes);
    Ok(())
}

/// Compares without stopping at the first difference, so the time taken
/// doesn't reveal how much of a guess was right.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// Runs `accept` against raw client bytes; returns the result and
    /// everything the server wrote.
    fn server_sees(client_bytes: &[u8], token: &str) -> (Result<Target, HandshakeError>, Vec<u8>) {
        runtime().block_on(async {
            let (mut client, mut server) = tokio::io::duplex(1024);
            client.write_all(client_bytes).await.unwrap();
            let result = accept(&mut server, token).await;
            drop(server);
            let mut written = Vec::new();
            client.read_to_end(&mut written).await.unwrap();
            (result, written)
        })
    }

    fn handshake(token: &str, request: &[u8]) -> Vec<u8> {
        let mut bytes = vec![5, 2, 0, 2, 1, 7];
        bytes.extend(b"gitaway");
        bytes.push(token.len() as u8);
        bytes.extend(token.as_bytes());
        bytes.extend(request);
        bytes
    }

    #[test]
    fn names_and_addresses_are_accepted_with_the_right_token() {
        let name = handshake(
            "secret",
            &[
                5, 1, 0, 3, 15, b'g', b'i', b't', b'.', b'e', b'x', b'a', b'm', b'p', b'l', b'e',
                b'.', b'c', b'o', b'm', 0, 22,
            ],
        );
        let (target, written) = server_sees(&name, "secret");
        assert_eq!(
            target.unwrap(),
            Target {
                host: "git.example.com".into(),
                port: 22
            }
        );
        assert_eq!(written, [5, 2, 1, 0]);

        let ipv4 = handshake("secret", &[5, 1, 0, 1, 192, 0, 2, 10, 0x08, 0xae]);
        assert_eq!(
            server_sees(&ipv4, "secret").0.unwrap(),
            Target {
                host: "192.0.2.10".into(),
                port: 2222
            }
        );

        let mut v6 = vec![5, 1, 0, 4];
        v6.extend("2001:db8::10".parse::<Ipv6Addr>().unwrap().octets());
        v6.extend([0, 22]);
        assert_eq!(
            server_sees(&handshake("secret", &v6), "secret")
                .0
                .unwrap()
                .host,
            "2001:db8::10"
        );
    }

    #[test]
    fn a_wrong_or_missing_token_is_refused_before_any_request() {
        let wrong = handshake("guess", &[5, 1, 0, 1, 192, 0, 2, 10, 0, 22]);
        let (result, written) = server_sees(&wrong, "secret");
        assert!(matches!(result, Err(HandshakeError::WrongToken)));
        assert_eq!(written, [5, 2, 1, 1]);

        let no_auth = [5, 1, 0];
        let (result, written) = server_sees(&no_auth, "secret");
        assert!(matches!(result, Err(HandshakeError::NoPassword)));
        assert_eq!(written, [5, 0xff]);
    }

    #[test]
    fn only_connect_is_supported() {
        let bind = handshake("secret", &[5, 2, 0, 1, 192, 0, 2, 10, 0, 22]);
        let (result, written) = server_sees(&bind, "secret");
        assert!(matches!(result, Err(HandshakeError::UnsupportedCommand)));
        assert_eq!(&written[4..6], [5, Reply::CommandNotSupported as u8]);
    }

    #[test]
    fn other_protocols_are_rejected() {
        let (result, _) = server_sees(b"GET / HTTP/1.1\r\n\r\n", "secret");
        assert!(matches!(result, Err(HandshakeError::NotSocks5)));
    }

    #[test]
    fn client_and_server_agree() {
        runtime().block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let target = accept(&mut stream, "secret").await.unwrap();
                reply(&mut stream, Reply::Succeeded).await.unwrap();
                stream.write_all(b"hello").await.unwrap();
                target
            });
            let client = tokio::task::spawn_blocking(move || {
                let mut stream = connect(proxy, "secret", "2001:db8::10", 22).unwrap();
                let mut hello = [0; 5];
                stream.read_exact(&mut hello).unwrap();
                hello
            });
            assert_eq!(&client.await.unwrap(), b"hello");
            assert_eq!(server.await.unwrap().host, "2001:db8::10");
        });
    }

    #[test]
    fn constant_time_eq_compares_whole_values() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b"a"));
    }
}
