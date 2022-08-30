This project provides some docker images and a docker compose setup to help running the node locally with less efforts. Before reading further make sure docker and docker compose are installed on your machine.

## Docker compose

You can use the Docker image [`farcasterd`](https://github.com/farcaster-project/farcaster-node/pkgs/container/farcaster-node%2Ffarcasterd) and images from the [`containers`](https://github.com/orgs/farcaster-project/packages?repo_name=containers) repo.

The easiest way is to use `docker-compose` to run both the node and the Monero wallet. Check our example in `compose/docker-compose.yml` file or clone the repo and run:

```
cd compose
docker-compose up -d
```

:mag_right: If you plan to make offer with the Docker stack make sure you understand what ports need to be open and reachable on your host and exposed from the container (port `9735` is exposed in the provided compose file). You might need to additionally configure your firewall to enable takers to connect.

To follow live `farcaster`'s logs run in another terminal (in the `compose/` folder):

```
docker-compose logs -f --no-log-prefix farcasterd
```

To interact with the node use `swap-cli` command line tool. The tool is installed in `farcasterd` container and already setup. To execute a command run:

```
docker-compose exec farcasterd swap-cli [command]
```

To stop and remove the containers simply run:

```
docker-compose down
```

Daemons used by the services are public testnet servers, you can change them by modifying `compose/farcaster.toml` file. After launching the containers you will see a new folder `compose/.farcaster` appears, this folder contains all node's data (nodeid key, database) and is stored on your machine so restarting the containers doesn't reset your node identity nor erase check-pointed data (public offers, swaps checkpoints, etc.)

### Images used

Images used in the docker compose are produced by the `farcaster-project` directly.

| Service      | Image                                                             |
| ------------ | ----------------------------------------------------------------- |
| `farcasterd` | `ghcr.io/farcaster-project/farcaster-node/farcasterd:latest`      |
| `walletrpc`  | `ghcr.io/farcaster-project/containers/monero-wallet-rpc:0.18.0.0` |

## Docker image

You can use the docker image directly with:

```
docker run --rm --init -t -p 9735\
    --mount type=bind,source=<config folder>,destination=/etc/farcaster\
    --name farcaster_node\
    ghcr.io/farcaster-project/farcaster-node/farcasterd:latest\
    -c /etc/farcaster/farcasterd.toml
```

If you don't use the default values for syncers daemons don't forget to mount your config inside the container (here at `/etc/farcaster`) and pass the config file with `-c /etc/farcaster/farcasterd.toml`.

or first build the node image locally and then start a container with `farcasterd:latest` instead of `ghcr.io/farcaster-project/farcaster-node/farcasterd:latest`:

```
docker build --no-cache -t farcasterd:latest .
```

The container will be removed after execution (`--rm`), allocate a pseudo-TTY (`-t`), and publish exposed port `9735` on the host in case you want to listens for incoming takers on the default port. If you only take public offers you can remove `-p 9735`.

Stop the container with `ctrl+c` or `docker stop farcaster_node`.

## Run the Monero RPC wallet

:mega: No monero are stored long-term in this wallet, the wallet is only used to detect locking and sweep to the specified external Monero address given in parameter at the beginning of the swap! Data needed to create this Monero wallet are managed by farcaster.

You need to run a Monero wallet RPC server locally to manage receiving and transferring moneros during a swap. You can run your own wallet RPC or use the following commands to create a temporary wallet RPC with one of our Docker image:

```
docker pull ghcr.io/farcaster-project/containers/monero-wallet-rpc:latest
docker run --rm -p 38083:38083 ghcr.io/farcaster-project/containers/monero-wallet-rpc:latest\
    /usr/bin/monero-wallet-rpc --stagenet\
    --disable-rpc-login --wallet-dir wallets\
    --daemon-host stagenet.community.rino.io:38081\
    --rpc-bind-ip 0.0.0.0 --rpc-bind-port 38083\
    --confirm-external-bind
```

Then use the following values in `farcasterd` configuration file:

| `monero_rpc_wallet` | value                    |
| ------------------- | ------------------------ |
| mainnet             | `http://localhost:18083` |
| stagenet            | `http://localhost:38083` |

Remove `--stagenet` add change `--rpc-bind-port` and `-p` values to `18083` and connect to a **mainnet** daemon host, see [:bulb: Use public infrastructure](./Home) for public mainnet daemon hosts.