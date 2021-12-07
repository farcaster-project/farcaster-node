# Run a swap locally

This document describe how to test a swap running both participant locally.

You'll need to build and install the node locally as described [here](../README.md#locally).

We'll create two node instances and will make them running a swap, for that make sure to have access to the three services: an `electrum server`, a `monero daemon`, and a `monero rpc wallet`.

## Run `monero rpc wallet` with Docker

:warning: You should run two containers, one for each `farcasterd` services, choose different ports and modify `--rpc-bind-port` and `-p` values accordingly. Pull the latest image and launch two containers:

```
docker pull ghcr.io/farcaster-project/containers/monero-wallet-rpc:latest
docker run --rm -p 38083:38083 ghcr.io/farcaster-project/containers/monero-wallet-rpc:latest\
    /usr/bin/monero-wallet-rpc --stagenet\
    --disable-rpc-login --wallet-dir wallets\
    --daemon-host stagenet.melo.tools:38081\
    --rpc-bind-ip 0.0.0.0 --rpc-bind-port 38083\
    --confirm-external-bind
```

and

```
docker run --rm -p 38084:38084 ghcr.io/farcaster-project/containers/monero-wallet-rpc:latest\
    /usr/bin/monero-wallet-rpc --stagenet\
    --disable-rpc-login --wallet-dir wallets\
    --daemon-host stagenet.melo.tools:38081\
    --rpc-bind-ip 0.0.0.0 --rpc-bind-port 38084\
    --confirm-external-bind
```

See [run `monero rpc wallet`](./docker-stack.md#run-monero-rpc-wallet) with Docker for more details.

## Launch `farcasterd` services

We will start two daemons with different data directories (`.node01` and `.node02`). First configure both node (with config files `.node01/farcasterd.toml` and `.node02/farcasterd.toml`) to use the two different monero rpc wallets you previously started and change the port for `monero_rpc_wallet` accordingly:

```toml
[syncers.testnet]
electrum_server = "ssl://blockstream.info:993"
monero_daemon = "http://stagenet.melo.tools:38081"
monero_rpc_wallet = "http://localhost:{38083|38084}"
```

Then launch two `farcasterd` services:

```
farcasterd -vv --data-dir {.node01|.node02}
```

You can use other public daemons for syncers, see [:bulb: Use public infrastructure](../README.md#bulb-use-public-infrastructure) for more details.

## Configure `swap-cli` clients

For simplicity you can create two aliases for the `swap-cli`, you will use them to make and take offers:

```
alias swap1-cli="swap-cli -d .node01"
alias swap2-cli="swap-cli -d .node02"
```

## Access to assets

You need to have access to Bitcoin (testnet) and Monero (stagenet) test coins, use your wallet of choice. You just need a way to fund the swap in bitcoins and moneros.

## Make the offer

Maker creates offer and start listening. Command used to print a serialized representation of the offer that shall be shared with taker. Additionally it spins up the listener awaiting for connection related to this offer (binded on `0.0.0.0:9735` by default with an offer public address of `127.0.0.1:9735` by default).

```
swap1-cli make --btc-addr tb1q4gj53tuew3e6u4a32kdtle2q72su8te39dpceq\
    --xmr-addr 54EYTy2HYFcAXwAbFQ3HmAis8JLNmxRdTC9DwQL7sGJd4CAUYimPxuQHYkMNg1EELNP85YqFwqraLd4ovz6UeeekFLoCKiu\
    --btc-amount "0.0000135 BTC"\
    --xmr-amount "0.001 XMR"
```

The command will create a public offer based on the chosen parameters, spin up a listening `peerd` (in that case `0.0.0.0:9735`), and return the encoded offer (starting with `Offer:...`).

This public offer should be shared by maker with taker. It also contains information on how to connect to maker (in that case `127.0.0.1:9735`). Additionally, it adds the public offers to the set of public offers in `farcasterd` that will be later used to initiate the swap upon takers requests.

To get a detailed explaination of make and more details on all possible arguments you can change run `swap1-cli make --help`.

## Take an offer

Taker accepts offer and connects to maker's daemon node with the following command (run `swap2-cli take --help` for more details):

Example of taking the offer above produced by maker:

```
swap2-cli take --btc-addr tb1qt3r3t6yultt8ne88ffgvgyym0sstj4apwsz05j\
    --xmr-addr 54EYTy2HYFcAXwAbFQ3HmAis8JLNmxRdTC9DwQL7sGJd4CAUYimPxuQHYkMNg1EELNP85YqFwqraLd4ovz6UeeekFLoCKiu\
    --offer Offer:...
```

The `--btc-addr` argument is the Bitcoin destination address in case the bitcoins needs to be refunded or if the swap completes (depending on the swap role), the `--xmr-addr` for Monero, and finally the offer with `--offer` or `-o`.

Upon taking the public offer is printed and user is asked for validation with `y` or `n`.

:mag_right: Flag of interest: `--without-validation` or `-w`, for externally validated automated setups (skip validation in cli).
