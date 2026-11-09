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

It starts the containers, creates a user and repository, sets gitaway up the way a user would (`profile add`, the server learns the device's key, `check` pins the host key through the tunnel), then:

- clones, pushes and pulls over `ssh://` and `http://` through the tunnel, with ssh checking the forge against the key gitaway pinned;
- checks that at "home" (`127.0.0.1`) commands connect directly, without the tunnel;
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
$ gitaway profile add home --endpoint 127.0.0.1:51820 \
    --server-public-key "$(docker compose exec -T wireguard cat /config/server.pub)" \
    --address 10.0.0.2/32 --home-ssh 192.0.2.10:2222 --host 192.0.2.10=10.0.0.1
$ docker compose exec wireguard add-peer.sh <the PublicKey that profile add printed> 10.0.0.2
$ gitaway check home --yes
```

Then create a repository in Forgejo at <http://127.0.0.1:3000> (sign in as `alice`), and use it through the tunnel:

```console
$ gitaway clone http://192.0.2.10:3000/alice/<repository>.git
```

Stop and delete everything with `docker compose down --volumes`, and `rm -rf /tmp/gitaway-playground`.
