# A home network in Docker

A WireGuard server and a [Forgejo](https://forgejo.org) forge in two containers, standing in for the network at home. Use it to try gitaway without a real home server, or to test changes end to end. CI runs [`e2e.sh`](e2e.sh) against it on every change.

```text
 your machine ──UDP 127.0.0.1:51820──▶ wireguard  (tunnel address 10.0.0.1)
                                          └─ forgejo, sharing its network:
                                               HTTP :3000, SSH :2222
```

Forgejo's ports are also published on `127.0.0.1`, which plays the part of "on the home network". Remote URLs use `192.0.2.10`, a documentation address nothing answers on, so commands that work with it must have gone through the tunnel.

You need Docker with Compose. The WireGuard server is `wireguard-go` on Alpine, so no WireGuard kernel module is needed.

## Run the end-to-end test

```console
$ cargo build
$ examples/home/e2e.sh
```

It starts the containers, creates a user and repository, and sets gitaway up the way a user would: the WireGuard server makes a config for the device (`new-device.sh`), and `gitaway setup` turns it into a profile, finding Forgejo through the tunnel and pinning its host key. Then it:

- clones, pushes and pulls over `ssh://` and `http://` through the tunnel, with ssh checking the forge against the key gitaway pinned;
- sets up a second profile at "home" (`127.0.0.1`), where `setup` finds Forgejo directly, and checks that commands there connect directly, without the tunnel;
- removes the device from the server and checks that the error says the VPN server never answered.

Everything it creates is in a temporary directory and a separate Compose project, removed at the end. Your own profiles, keychain and git config aren't touched. To test another build, set `GITAWAY=/path/to/gitaway`.

## Try it by hand

Use a separate gitaway configuration, so the playground profile and its key stay out of your own:

```console
$ cd examples/home
$ docker compose up --detach --build
$ docker compose exec forgejo forgejo admin user create --username alice \
    --password correct-horse-battery --email alice@example.com --must-change-password=false
$ export XDG_CONFIG_HOME=/tmp/gitaway-playground GITAWAY_KEY_STORE=file
$ docker compose exec -T wireguard new-device.sh 10.0.0.2 > /tmp/gitaway-playground.conf
$ gitaway setup --config /tmp/gitaway-playground.conf --url ssh://git@192.0.2.10:2222/alice/app.git
```

`new-device.sh` does what a home WireGuard server does when you add a device: it makes the device's keys, adds it as a peer, and prints its config. To try the other path, where gitaway makes the key, use `gitaway profile add` and give the server the key `gitaway peer` prints with `docker compose exec wireguard add-peer.sh <public key> 10.0.0.2`.

Then create a repository in Forgejo at <http://127.0.0.1:3000> (sign in as `alice`), and use it through the tunnel:

```console
$ gitaway clone http://192.0.2.10:3000/alice/<repository>.git
```

Stop and delete everything with `docker compose down --volumes`, and `rm -rf /tmp/gitaway-playground /tmp/gitaway-playground.conf`.
