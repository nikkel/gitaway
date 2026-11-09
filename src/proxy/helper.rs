//! `gitaway __proxy-connect <host> <port>`: connects stdin and stdout to
//! `host:port` through the running proxy.
//!
//! ssh runs it as its `ProxyCommand`, which expects a program that becomes
//! the connection: whatever it reads from stdin goes to the server, and whatever the server
//! sends comes out on stdout.
//!
//! ```text
//!  ssh ──stdin──▶ helper ──▶ proxy (SOCKS5, token) ──▶ tunnel or direct ──▶ server
//!      ◀─stdout── helper ◀──
//! ```
//!
//! The proxy's address and token come from the environment
//! ([`super::PROXY_ENV`], [`super::TOKEN_ENV`]), set by gitaway for git and
//! inherited by ssh. The helper is a separate, short-lived process, so it
//! uses plain blocking I/O and two threads rather than an async runtime.

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr};

use super::{PROXY_ENV, TOKEN_ENV, socks};

/// The hidden command name. The leading underscores keep it out of the way
/// of git commands and aliases.
pub const COMMAND: &str = "__proxy-connect";

/// Runs the helper with its arguments (host and port). Returns the exit code.
pub fn run(args: &[String]) -> i32 {
    match connect_and_relay(args) {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("gitaway: {message}");
            1
        }
    }
}

fn connect_and_relay(args: &[String]) -> Result<(), String> {
    let [host, port] = args else {
        return Err(format!("usage: gitaway {COMMAND} <host> <port>"));
    };
    let port: u16 = port
        .parse()
        .map_err(|_| format!("`{port}` isn't a port number"))?;
    let proxy: SocketAddr = std::env::var(PROXY_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| {
            format!("{COMMAND} only works inside a command gitaway runs ({PROXY_ENV} isn't set)")
        })?;
    let token = std::env::var(TOKEN_ENV).map_err(|_| format!("{TOKEN_ENV} isn't set"))?;

    let stream = socks::connect(proxy, &token, host, port)
        .map_err(|e| format!("couldn't reach {host}:{port}: {e}"))?;
    relay(stream).map_err(|e| format!("connection to {host}:{port}: {e}"))
}

/// Copies stdin to the connection and the connection to stdout, until the
/// server closes it.
fn relay(stream: std::net::TcpStream) -> io::Result<()> {
    let mut to_server = stream.try_clone()?;
    std::thread::spawn(move || {
        let _ = io::copy(&mut io::stdin().lock(), &mut to_server);
        // Tell the server nothing more is coming, but keep reading its answer.
        let _ = to_server.shutdown(Shutdown::Write);
    });
    let mut from_server = stream;
    let mut stdout = io::stdout().lock();
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = from_server.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        stdout.write_all(&buf[..n])?;
        stdout.flush()?;
    }
}
