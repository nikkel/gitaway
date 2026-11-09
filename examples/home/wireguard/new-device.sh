#!/bin/sh
# Adds a new device to the WireGuard server and prints its client config,
# the way home servers and WireGuard apps hand one out:
#   docker compose exec -T wireguard new-device.sh 10.0.0.2 > laptop.conf
#   gitaway setup --config laptop.conf
# The config holds the device's private key; only the public key stays here.
set -eu
[ $# -eq 1 ] || { echo "usage: new-device.sh <tunnel address>" >&2; exit 2; }
: "${WG_ENDPOINT:?set WG_ENDPOINT to the public host:port of this server}"

private=$(wg genkey)
public=$(printf '%s\n' "$private" | wg pubkey)
wg set wg0 peer "$public" allowed-ips "$1/32"
echo "Added a device at $1. Its config is below; give it to the device." >&2

cat <<CONFIG
[Interface]
PrivateKey = $private
Address = $1/32

[Peer]
PublicKey = $(cat /config/server.pub)
Endpoint = $WG_ENDPOINT
AllowedIPs = 10.0.0.1/32
CONFIG
