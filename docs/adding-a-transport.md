# Adding a transport

A *transport* is how gitaway reaches the home network when you're away. WireGuard is the first one. This guide shows how to add another, such as OpenVPN.

## What a transport has to do

Exactly one thing: open a TCP connection to an address on the home network.

```rust
pub trait Transport: Send + Sync {
    fn connect(&self, target: SocketAddr) -> ConnectFuture<'_>;
}
```

`target` is an address as seen from inside the home network: the right-hand side of a profile's `[hosts]` entry, such as `10.0.0.1:2222`. The result is a byte stream (`AsyncRead + AsyncWrite`). Dropping the transport must close the tunnel and stop any background tasks.

Nothing else in gitaway knows which transport is in use. The local proxy that git talks to, profile selection and the home check all go through `connect`, so a new transport doesn't touch them.

```text
 git ─▶ local proxy ─▶ dyn Transport::connect(10.0.0.1:2222)
                          ├─ WireGuardTransport  (src/transport/wireguard.rs)
                          └─ YourTransport       (src/transport/your_vpn.rs)
```

## Reusing the network stack

Most VPNs carry raw IP packets, not TCP connections. To turn "connect to 10.0.0.1:2222" into IP packets without a virtual network adapter or root, gitaway has its own TCP/IP stack in userspace: `src/transport/netstack.rs`, built on `smoltcp`.

```rust
let (stack, pipe) = NetStack::new(tunnel_address, mtu);
// pipe.outbound: IP packets the stack wants sent through your VPN
// pipe.inbound:  IP packets your VPN received, for the stack
let stream = stack.connect(target).await?;
```

Your transport moves packets between `pipe` and the VPN library, and its `connect` calls `stack.connect`. `wireguard.rs` is a complete example in about 150 lines; most of it is glue between `pipe` and gotatun.

A transport that already yields TCP streams (an SSH jump host, a SOCKS server) doesn't need the stack: its `connect` returns those streams directly.

## Steps

1. **Settings.** Add a variant to `Transport` in `src/config/profile.rs` with a struct for its settings (`#[serde(deny_unknown_fields)]`), and validate it in `Profile::validate`. The TOML table name is the variant's name: `[transport.openvpn]`. Add tests that it parses and that invalid values are rejected.
2. **The transport.** Add `src/transport/<name>.rs` with a type that implements `Transport`, and an `open` function that starts the tunnel. Start with a `//!` comment explaining how it works, as `wireguard.rs` does.
3. **Wire it up.** Add an arm to `open` in `src/transport/mod.rs`, and to any `match` on the profile's transport that the compiler points out (`profile list`, `peer`).
4. **Keys.** If the VPN uses a per-device secret, keep it with `src/keys/` (the OS keychain or a private file), never in the profile.
5. **Tests.** Test it in-process where possible, with a server on `127.0.0.1` inside the test, as `wireguard.rs` does: a real handshake and real data, but no root and no network.
6. **Docs.** Document the settings in [`configuration.md`](configuration.md), and update this guide if something here was unclear.

## Rules

- **No secrets in arguments or the environment.** A child process would expose its arguments to other users through `ps`. Prefer running the VPN in-process.
- **No root, no system-wide changes.** The tunnel lives and dies with the gitaway process.
- **Fail clearly.** Map failures to `TransportError` variants with a message that says what to check, for example that the server is reachable and knows this device.
- **TCP only.** git needs nothing else. The proxy only asks for TCP connections.
