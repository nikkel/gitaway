#!/bin/sh
# Adds a device whose key was made elsewhere (`gitaway profile add`, then
# `gitaway peer`) to the WireGuard server:
#   docker compose exec wireguard add-peer.sh <device public key> 10.0.0.2
# For a device without a key yet, new-device.sh is simpler.
#
# The device is also written to /config/peers, which start.sh replays, so it
# survives a restart. Adding a key again replaces its address.
set -eu
[ $# -eq 2 ] || { echo "usage: add-peer.sh <public key> <tunnel address>" >&2; exit 2; }
wg set wg0 peer "$1" allowed-ips "$2/32"
umask 077
touch /config/peers
grep -v -F "$1 " /config/peers >/config/peers.new || true
printf '%s %s\n' "$1" "$2" >>/config/peers.new
mv /config/peers.new /config/peers
echo "Added peer $1 at $2."
