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
# wireguard-go creates the interface before its control socket. `wg` only
# talks to it through that socket (without it, wg tries the kernel's
# WireGuard and fails), so wait for the socket, not just the interface.
tries=0
until ip link show wg0 >/dev/null 2>&1 && [ -S /var/run/wireguard/wg0.sock ]; do
    kill -0 "$daemon" 2>/dev/null || { echo "wireguard-go exited during startup" >&2; exit 1; }
    tries=$((tries + 1))
    [ "$tries" -le 100 ] || { echo "wg0 wasn't ready within 10 seconds" >&2; exit 1; }
    sleep 0.1
done
wg set wg0 listen-port 51820 private-key /config/server.key
ip address add 10.0.0.1/24 dev wg0
ip link set wg0 mtu 1420 up

# Devices added before a restart (add-peer.sh keeps the list).
if [ -f /config/peers ]; then
    while read -r key address; do
        wg set wg0 peer "$key" allowed-ips "$address/32"
    done </config/peers
fi
echo "WireGuard server up. Public key: $(cat /config/server.pub)"

# The container lives as long as wireguard-go does.
wait "$daemon"
