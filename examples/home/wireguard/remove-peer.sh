#!/bin/sh
# Removes a device from the WireGuard server, and from /config/peers so it
# stays removed after a restart:
#   docker compose exec wireguard remove-peer.sh <device public key>
set -eu
[ $# -eq 1 ] || { echo "usage: remove-peer.sh <public key>" >&2; exit 2; }
wg set wg0 peer "$1" remove
if [ -f /config/peers ]; then
    grep -v -F "$1 " /config/peers >/config/peers.new || true
    mv /config/peers.new /config/peers
fi
echo "Removed peer $1."
