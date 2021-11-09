# How to use `farcasterd` with Docker

This project provide some docker images and a docker compose setup to help running the node locally with less efforts. Before reading further make sure docker and docker compose are installed on your machine.

## Docker compose

To launch `farcasterd` on your machine clone this project and run:

```
docker-compose up -d
docker-compose logs farcasterd -f --no-log-prefix
```

These commands launch a local Monero Wallet RPC and the Farcaster node, then follow the logs produced by `farcasterd`. Daemons used by the services are public testnet servers.

Install `swap-cli` on your machine (follow the documentation on [build and run locally](../README.md#locally)) and run:

```
swap-cli -x "lnpz://127.0.0.1:9981/?api=esb" info
```

You should get a returned value from the node. You can use this stack to take a public offer or to make an offer. In the case you want to make an offer make sure the public address and the port will be reachable from external networks.

:mega: The only port forwarded to the host is `9735`, make sure to use this one when making offers.

To stop and remove the containers simply run in the project folder:

```
docker-compose down
```

### Images used

Images used in the docker compose are produced by the `farcaster-project` directly.

| Service      | Image                                                             |
| ------------ | ----------------------------------------------------------------- |
| `farcasterd` | `ghcr.io/farcaster-project/farcaster-node/farcasterd:latest`      |
| `walletrpc`  | `ghcr.io/farcaster-project/containers/monero-wallet-rpc:0.17.2.3` |

## Docker image

You can use the docker image produced directly by the GitHub CI with:

```
docker run --rm -t -p 9735:9735 -p 9981:9981\
    --name farcaster_node\
    ghcr.io/farcaster-project/farcaster-node/farcasterd:latest\
    -vv\
    --electrum-server {ip:port}\
    --monero-daemon http://{ip:port}\
    --monero-rpc-wallet http://{ip:port}
```

or build the node image and start a container by running inside the project folder:

```
docker build -t farcasterd:latest .
docker run --rm -t -p 9735:9735 -p 9981:9981\
    --name farcaster_node\
    farcasterd:latest\
    ...
```

The container will be removed after execution (`--rm`), allocate a pseudo-TTY (`-t`), and publish exposed ports `9735` and `9981` on the host.

Stop the container with `docker stop farcaster_node` (ctrl+c does not work yet).

:warning: this exposes the control bus on the host, only intended for debugging or on a trusted network.

## Connect a client

To connect a client and command `farcasterd` running inside a Docker container simply run:

```
swap-cli -x "lnpz://127.0.0.1:9981/?api=esb" info
```

This configure the cli to connects to the exposed port `9981` of the running container `farcasterd`.

### Remote client usage

Example from using other URLs supported by crate `internet2` `node_addr.rs`, besides the default inter process communication (also used by the Docker image to expose the control bus).

The daemon is controlled though ZMQ _ctl_ socket, an internal interface for control PRC protocol communications. Another ZMQ socket is used to forward all incoming protocol messages, the _msg_ socket. Both are node internal communication channels. Message from counterparty come through `peerd` services.

**Farcasterd**

To launch `farcasterd` with network binded _control_ (`-x`) bus and _message_ bus (`-m`) instead of `ctl.rpc` and `msg.rpc` files:

```
farcasterd -vv -x "lnpz://127.0.0.1:9981/?api=esb" -m "lnpz://127.0.0.1:9982/?api=esb"
```

**Client**

The following client can instruct the above `farcasterd` to return general informations as follows:

```
swap-cli -x "lnpz://127.0.0.1:9981/?api=esb" -m "lnpz://127.0.0.1:9982/?api=esb" info
```

:mega: It is worth noting that _ctl_ and _msg_ sockets can use different type of communication. E.g. the docker stack only binds the _ctl_ socket over the network and keeps the _msg_ to its default `msg.rpc` file inside the data directory.
