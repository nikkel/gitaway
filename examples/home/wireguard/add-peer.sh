#!/bin/sh
# Adds a device to the WireGuard server, as `gitaway peer` asks you to:
#   docker compose exec wireguard add-peer.sh <device public key> 10.0.0.2
set -eu
[ $# -eq 2 ] || { echo "usage: add-peer.sh <public key> <tunnel address>" >&2; exit 2; }
wg set wg0 peer "$1" allowed-ips "$2/32"
echo "Added peer $1 at $2."
