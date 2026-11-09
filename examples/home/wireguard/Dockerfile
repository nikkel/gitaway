# A WireGuard server standing in for the one on a home network. It runs
# wireguard-go, WireGuard as an ordinary program, so it works wherever
# Docker does, without a WireGuard kernel module.
FROM alpine:3.22
RUN apk add --no-cache wireguard-go wireguard-tools iproute2
COPY start.sh add-peer.sh /usr/local/bin/
ENTRYPOINT ["/usr/local/bin/start.sh"]
