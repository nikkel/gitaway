#!/bin/sh
# Adds a device whose key was made elsewhere (`gitaway profile add`, then
# `gitaway peer`) to the WireGuard server:
#   docker compose exec wireguard add-peer.sh <device public key> 10.0.0.2
# For a device without a key yet, new-device.sh is simpler.
set -eu
[ $# -eq 2 ] || { echo "usage: add-peer.sh <public key> <tunnel address>" >&2; exit 2; }
wg set wg0 peer "$1" allowed-ips "$2/32"
echo "Added peer $1 at $2."
