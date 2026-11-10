# WireGuard in five minutes

gitaway reaches your home through a WireGuard server. You don't need to understand WireGuard to use `gitaway setup`, but it helps when something doesn't work. This page explains the parts in plain words. All addresses and names are examples.

## The picture

```text
 laptop, away from home                                   home
 ┌───────────────────┐                     ┌──────────────────────────────────┐
 │ gitaway           │   encrypted UDP     │ router: forwards UDP port 51820  │
 │ tunnel address    │ ──────────────────▶ │   └─▶ WireGuard server           │
 │ 10.0.0.2          │  to vpn.example.net │       tunnel address 10.0.0.1    │
 └───────────────────┘        :51820       │         └─▶ git server           │
                                           │             (192.0.2.10 at home) │
                                           └──────────────────────────────────┘
```

Your laptop sends encrypted packets over the internet to your home router, which passes them to the WireGuard server. Inside that encrypted *tunnel*, the laptop and the server have their own small private network, separate from your home network.

## Keys: two pairs, no passwords

WireGuard has no usernames or passwords. Each side has a **key pair**:

- a **private key**, which is secret and never leaves its machine;
- a **public key**, worked out from the private one, which is safe to share.

```text
 this device                              WireGuard server
 private key  (stays here)                private key  (stays there)
 public key  ─────── server is told ────▶ list of devices ("peers")
 server's public key ◀── device is told ─ public key
```

Each side is told the other's public key, and only talks to the keys it knows. Adding a device to the server means adding its public key, with the tunnel address it gets, to the server's list of *peers*. Removing it means taking it off the list.

## The config file, line by line

When a server adds a device for you, it gives you a config file for it:

```ini
[Interface]                 # about this device
PrivateKey = …              # this device's private key: keep it secret
Address = 10.0.0.2/32       # this device's address inside the tunnel

[Peer]                      # about the server
PublicKey = …               # the server's public key
Endpoint = vpn.example.net:51820   # where to find the server on the internet
AllowedIPs = 10.0.0.1/32    # which addresses go through the tunnel
```

| Line | What it is | Where it comes from |
|---|---|---|
| `PrivateKey` | This device's secret key | Made by the server or app when it added the device |
| `Address` | This device's address in the tunnel | Picked by the server, one per device |
| `PublicKey` (under `[Peer]`) | The server's public key | The server's own key, the same for every device |
| `Endpoint` | Your home's address on the internet, and the UDP port | Usually a dynamic DNS name, because home internet addresses change |
| `AllowedIPs` | Which addresses are reached through the tunnel | `10.0.0.1/32` means only the server itself; `0.0.0.0/0` means everything |

gitaway uses the config differently from the WireGuard app: only gitaway's own git connections go through the tunnel, and only while a command runs. The rest of your laptop's traffic is untouched, and `AllowedIPs` only helps `setup` guess where your git server is inside the tunnel.

## What the server side needs

- **A WireGuard server** that can reach your git server: running on the same machine, or on the same home network.
- **A port forward** on your router: the `Endpoint`'s UDP port, to the WireGuard server.
- **A dynamic DNS name**, so the `Endpoint` keeps working when your home's internet address changes. Many routers and NAS systems include one.

## Common mix-ups

- **Two public keys in one config screen.** WireGuard apps show the device's own public key at the top. gitaway needs the server's, the `PublicKey` under `[Peer]`. If you set a profile by hand and mix them up, gitaway says so.
- **One key on two devices.** A server keeps one connection per key, so two devices sharing a config knock each other off. Give each device its own.
- **Testing from home.** From inside your home network, the tunnel has to go out to your router's public address and back in. Many routers can't do that ("hairpin NAT"), so test the tunnel from another network, such as your phone's hotspot.
- **Preshared keys.** Some servers, such as wg-easy and PiVPN, add a `PresharedKey` line under `[Peer]`: an optional extra secret the server and this device share. Both sides need the same one. `setup` reads it from the config and keeps it with the device key, so there's nothing to do.
