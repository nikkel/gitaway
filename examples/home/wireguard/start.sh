#!/bin/sh
# Starts the home WireGuard server: tunnel address 10.0.0.1, UDP port 51820.
# The server's key is made on first start and kept in /config, with its
# public half in /config/server.pub for gitaway's profile.
set -eu

umask 077
mkdir -p /config
[ -f /config/server.key ] || wg genkey > /config/server.key
wg pubkey < /config/server.key > /config/server.pub
chmod 644 /config/server.pub

WG_PROCESS_FOREGROUND=1 wireguard-go wg0 &
daemon=$!
while ! ip link show wg0 >/dev/null 2>&1; do sleep 0.1; done
wg set wg0 listen-port 51820 private-key /config/server.key
ip address add 10.0.0.1/24 dev wg0
ip link set wg0 mtu 1420 up
echo "WireGuard server up. Public key: $(cat /config/server.pub)"

# The container lives as long as wireguard-go does.
wait "$daemon"
