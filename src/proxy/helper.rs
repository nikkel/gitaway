//! `gitaway __proxy-connect <host> <port> [<original name>]`: connects
//! stdin and stdout to `host:port` through the running proxy.
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
//! ssh passes the host after applying `~/.ssh/config` (`%h`: an alias's
//! `Hostname`), and the name it was given (`%n`: the alias itself). A
//! profile's `[hosts]` may name either, so when they differ the helper asks
//! the proxy for the original name first, as a home host only, and falls
//! back to `%h` only if that isn't one:
//!
//! ```text
//!  ssh: git@nas, with "Host nas / Hostname 192.0.2.10 / Port 2222"
//!   → helper 192.0.2.10 2222 nas
//!       1. home only: nas:2222        in [hosts]? ─ yes ─▶ through the tunnel
//!       2. otherwise: 192.0.2.10:2222  as usual (tunnel if in [hosts], else direct)
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

/// Runs the helper with its arguments (host, port, and optionally the name
/// ssh was given). Returns the exit code.
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
    let (host, port, original) = match args {
        [host, port] => (host, port, None),
        [host, port, original] => (host, port, Some(original)),
        _ => {
            return Err(format!(
                "usage: gitaway {COMMAND} <host> <port> [<original name>]"
            ));
        }
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

    let unreachable = |name: &str, e: io::Error| format!("couldn't reach {name}:{port}: {e}");
    let home_by_original_name = match original.filter(|name| !name.eq_ignore_ascii_case(host)) {
        Some(name) => {
            socks::connect_home_only(proxy, &token, name, port).map_err(|e| unreachable(name, e))?
        }
        None => None,
    };
    let stream = match home_by_original_name {
        Some(stream) => stream,
        None => socks::connect(proxy, &token, host, port).map_err(|e| unreachable(host, e))?,
    };
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
