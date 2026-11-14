#!/usr/bin/env bash
# End-to-end test: the real gitaway binary against a real Forgejo behind a
# real WireGuard server (compose.yaml), the way a user sets it up.
#
#   ./e2e.sh                      uses ../../target/debug/gitaway
#   GITAWAY=/path/to/gitaway ./e2e.sh
#
# Needs docker (with compose), curl, git and OpenSSH. Everything it creates
# lives in a temporary directory and the compose project, both removed at
# the end. Your own gitaway profiles, keychain and git config aren't touched.
#
# The remote URLs use 192.0.2.10, a documentation address nothing answers
# on, so commands can only work through the tunnel. 127.0.0.1 plays the part
# of "on the home network", where they go directly.

set -euo pipefail
cd "$(dirname "$0")"

GITAWAY=${GITAWAY:-$(cd ../.. && pwd)/target/debug/gitaway}
PASSWORD=correct-horse-battery
SSH_URL=ssh://git@192.0.2.10:2222/alice/app.git
HTTP_URL=http://192.0.2.10:3000/alice/app.git

work=$(mktemp -d)
compose() { docker compose --project-name gitaway-e2e "$@"; }
cleanup() {
    status=$?
    if [ "$status" -ne 0 ]; then
        echo "--- wireguard logs" >&2
        compose logs --tail 20 wireguard >&2 || true
    fi
    compose down --volumes --timeout 2 >/dev/null 2>&1 || true
    rm -rf "$work"
    exit "$status"
}
trap cleanup EXIT

step() { printf '\n== %s\n' "$*"; }
fail() { echo "FAILED: $*" >&2; exit 1; }
# Runs a command that must succeed and print something matching a pattern.
expect() {
    local pattern=$1
    shift
    local out status=0
    out=$("$@" 2>&1) || status=$?
    if [ "$status" -ne 0 ]; then
        echo "$out" >&2
        fail "$* exited with $status"
    fi
    echo "$out"
    grep -q -- "$pattern" <<<"$out" || fail "expected '$pattern' from: $*"
}

# A private environment for gitaway, git and ssh.
export XDG_CONFIG_HOME=$work/config
export GITAWAY_KEY_STORE=file
export GIT_CONFIG_NOSYSTEM=1
export GIT_CONFIG_GLOBAL=$work/gitconfig
git config --global user.name Alice
git config --global user.email alice@example.com
git config --global init.defaultBranch main
git config --global credential.helper "!f() { echo username=alice; echo password=$PASSWORD; }; f"
ssh-keygen -q -t ed25519 -N '' -C alice@example.com -f "$work/id_ed25519"
export GIT_SSH_COMMAND="ssh -F /dev/null -i $work/id_ed25519 -o IdentitiesOnly=yes \
-o BatchMode=yes -o UserKnownHostsFile=$work/known_hosts -o StrictHostKeyChecking=yes"

step "Start the home network"
compose up --detach --build --quiet-pull
for _ in $(seq 60); do
    curl -fs http://127.0.0.1:3000/api/healthz >/dev/null && break
    sleep 1
done
curl -fs http://127.0.0.1:3000/api/healthz >/dev/null || fail "Forgejo didn't start"
compose exec -T forgejo forgejo admin user create --username alice \
    --password "$PASSWORD" --email alice@example.com --must-change-password=false
api() { curl -fs -u "alice:$PASSWORD" -H 'Content-Type: application/json' "$@"; }
api -d '{"name":"app","auto_init":true,"default_branch":"main"}' \
    http://127.0.0.1:3000/api/v1/user/repos >/dev/null
api -d "{\"title\":\"e2e\",\"key\":\"$(cat "$work/id_ed25519.pub")\"}" \
    http://127.0.0.1:3000/api/v1/user/keys >/dev/null

step "Set up: the server makes this device's config, gitaway setup does the rest"
compose exec -T wireguard new-device.sh 10.0.0.2 >"$work/laptop.conf"
expect "Found your git server at 10.0.0.1:2222" \
    "$GITAWAY" setup home --config "$work/laptop.conf" --url "$SSH_URL"
device_key=$("$GITAWAY" peer home 2>/dev/null | sed -n 's/^PublicKey = //p')

step "check: the tunnel works and the pinned host key matches"
expect "It matches" "$GITAWAY" check home
host_key=$(sed -n 's/^host_key = "\(.*\)"$/\1/p' "$XDG_CONFIG_HOME/gitaway/profiles/home.toml")
[ -n "$host_key" ] || fail "no host key pinned"
# ssh itself checks the forge against the key gitaway pinned.
printf '[192.0.2.10]:2222 %s\n[127.0.0.1]:2222 %s\n' "$host_key" "$host_key" >"$work/known_hosts"

cd "$work"
step "Away: clone, push and pull over ssh:// and http:// through the tunnel"
"$GITAWAY" clone "$SSH_URL" ssh-clone
[ "$(git -C ssh-clone config gitaway.profile)" = home ] || fail "clone didn't remember its profile"
out=$(GIT_TRACE=1 "$GITAWAY" -C ssh-clone ls-remote origin 2>&1) \
    || { echo "$out" >&2; fail "ls-remote through the tunnel failed"; }
grep -q ProxyCommand <<<"$out" || fail "ssh didn't go through gitaway's proxy"
echo "over ssh" >ssh-clone/ssh.txt
git -C ssh-clone add ssh.txt
git -C ssh-clone commit -qm "push over ssh"
"$GITAWAY" -C ssh-clone push

"$GITAWAY" clone "$HTTP_URL" http-clone
[ -f http-clone/ssh.txt ] || fail "the http clone is missing the commit pushed over ssh"
echo "over http" >http-clone/http.txt
git -C http-clone add http.txt
git -C http-clone commit -qm "push over http"
"$GITAWAY" -C http-clone push
"$GITAWAY" -C ssh-clone pull --ff-only
[ -f ssh-clone/http.txt ] || fail "pull over ssh didn't bring the commit pushed over http"
[ "$(git -C ssh-clone remote get-url origin)" = "$SSH_URL" ] || fail "the remote URL changed"

step "Away: the web interface on this computer with gitaway tunnel"
"$GITAWAY" profile set home web.url http://192.0.2.10:3000 >/dev/null
"$GITAWAY" tunnel home --port 18080 >"$work/tunnel.log" 2>&1 &
tunnel_pid=$!
for _ in $(seq 30); do
    grep -q "is up" "$work/tunnel.log" && break
    sleep 1
done
grep -q "is up" "$work/tunnel.log" || { cat "$work/tunnel.log" >&2; fail "the tunnel didn't come up"; }
curl -fs http://localhost:18080/api/healthz | grep -q pass \
    || { cat "$work/tunnel.log" >&2; fail "the web interface didn't answer through the tunnel"; }
# A web page using its own name for the port (DNS rebinding) is refused.
status=$(curl -s -o /dev/null -w '%{http_code}' -H 'Host: evil.example:18080' http://127.0.0.1:18080/api/healthz)
[ "$status" = 421 ] || fail "gitaway tunnel answered a request for another site's name ($status)"
# Other commands use the running tunnel instead of waiting for it to stop.
echo "while the tunnel runs" >ssh-clone/tunnel.txt
git -C ssh-clone add tunnel.txt
git -C ssh-clone commit -qm "push while gitaway tunnel runs"
out=$("$GITAWAY" -C ssh-clone push 2>&1) || { echo "$out" >&2; fail "push during gitaway tunnel failed"; }
grep -q "using the running" <<<"$out" || { echo "$out" >&2; fail "push didn't use the running tunnel"; }
out=$("$GITAWAY" -C http-clone pull --ff-only 2>&1) || { echo "$out" >&2; fail "pull during gitaway tunnel failed"; }
[ -f http-clone/tunnel.txt ] || fail "pull over http didn't bring the commit pushed during the tunnel"
kill -TERM "$tunnel_pid"
wait "$tunnel_pid" || fail "gitaway tunnel didn't stop cleanly"
grep -q "Tunnel closed" "$work/tunnel.log" || fail "gitaway tunnel didn't say it closed"
[ ! -e "$XDG_CONFIG_HOME/gitaway/runs/home.toml" ] || fail "gitaway tunnel left its runs file behind"

step "Home: with the forge on the local network, commands go directly"
expect "Tunnel: not tested" "$GITAWAY" setup lan --yes --config "$work/laptop.conf" \
    --url ssh://git@127.0.0.1:2222/alice/app.git
expect "you're home" "$GITAWAY" check lan
out=$(GIT_TRACE=1 "$GITAWAY" ls-remote ssh://git@127.0.0.1:2222/alice/app.git 2>&1) \
    || { echo "$out" >&2; fail "ls-remote at home failed"; }
grep -q refs/heads/main <<<"$out" || fail "ls-remote at home listed no branches"
! grep -q ProxyCommand <<<"$out" || fail "at home, ssh went through the proxy"

step "The server remembers its devices across a restart"
# forgejo shares the wireguard container's network, so it stops first and
# starts again after it.
compose stop forgejo >/dev/null
compose restart wireguard >/dev/null
compose start forgejo >/dev/null
for _ in $(seq 60); do
    curl -fs http://127.0.0.1:3000/api/healthz >/dev/null && break
    sleep 1
done
"$GITAWAY" -C ssh-clone fetch || fail "after a restart, the server no longer knew this device"

step "A server that no longer knows this device is reported as such"
compose exec -T wireguard remove-peer.sh "$device_key" >/dev/null
if out=$("$GITAWAY" -C ssh-clone fetch 2>&1); then
    fail "fetch worked without the server knowing this device"
fi
echo "$out"
grep -q "no answer from the VPN server" <<<"$out" || fail "the error didn't say the VPN server never answered"

step "All end-to-end checks passed"
