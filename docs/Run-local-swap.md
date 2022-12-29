This document describes how to test a swap running both participants locally.

You'll need to [build and install](./Install-guide) the node locally.

We'll create two node instances and will make them running a testnet swap, for that make sure to have access to the three services: an `electrum server` (can use a public one from default config), a `monero daemon` (can use a public one from default config), and a `monero rpc wallet` (needs to run locally).

## Run monero rpc wallet with Docker

:warning: You should run two `monero rpc wallet`, one for each `farcasterd` services, choose different ports and modify `--rpc-bind-port` and `-p` values accordingly. Pull the latest image and launch two containers:

```
docker pull ghcr.io/farcaster-project/containers/monero-wallet-rpc:latest
docker run --rm -p 38083:38083 ghcr.io/farcaster-project/containers/monero-wallet-rpc:latest\
    /usr/bin/monero-wallet-rpc --stagenet\
    --disable-rpc-login --wallet-dir wallets\
    --daemon-host stagenet.community.rino.io:38081\
    --rpc-bind-ip 0.0.0.0 --rpc-bind-port 38083\
    --confirm-external-bind
```

and

```
docker run --rm -p 38084:38084 ghcr.io/farcaster-project/containers/monero-wallet-rpc:latest\
    /usr/bin/monero-wallet-rpc --stagenet\
    --disable-rpc-login --wallet-dir wallets\
    --daemon-host stagenet.community.rino.io:38081\
    --rpc-bind-ip 0.0.0.0 --rpc-bind-port 38084\
    --confirm-external-bind
```

See [run the Monero rpc wallet](./Using-Docker#run-the-monero-rpc-wallet) with Docker for more details.

## Launch farcasterd services

We will start two daemons with different data directories (`.node01` and `.node02`). First configure both node (with config files `.node01/farcasterd.toml` and `.node02/farcasterd.toml`) to use the two different monero rpc wallets you previously started and change the port for `monero_rpc_wallet` accordingly:

```toml
[syncers.testnet]
electrum_server = "ssl://blockstream.info:993"
monero_daemon = "http://stagenet.community.rino.io:38081"
monero_rpc_wallet = "http://localhost:{38083|38084}"
```

Then launch two `farcasterd` services:

```
farcasterd --data-dir {.node01|.node02}
```

You can use other public daemons for configuring your syncers, see the default values in [:bulb: Use public infrastructure](./Home#bulb-use-public-infrastructure).

## Configure swap-cli clients

For simplicity you can create two aliases for the `swap-cli`, you will use them to make and take deals:

```
alias swap1-cli="swap-cli -d .node01"
alias swap2-cli="swap-cli -d .node02"
```

## Access to assets

You need to have access to Bitcoin (testnet) and Monero (stagenet) test coins, use your wallet of choice. You just need a way to fund the swap in bitcoins and moneroj.

## Make a deal

Maker creates deals. The serialized representation (like an address but longer) of the deal shall be shared with a taker.

```
swap1-cli make --btc-addr tb1q4gj53tuew3e6u4a32kdtle2q72su8te39dpceq\
    --xmr-addr 54EYTy2HYFcAXwAbFQ3HmAis8JLNmxRdTC9DwQL7sGJd4CAUYimPxuQHYkMNg1EELNP85YqFwqraLd4ovz6UeeekFLoCKiu\
    --btc-amount "0.0000135 BTC"\
    --xmr-amount "0.001 XMR"
```

The command will create a deal based on the chosen parameters, spin up a listening `peerd` (bound to `0.0.0.0:7067` on your machine by default), and return the encoded deal (starting with `Deal:...`).

This deal should be shared by maker with taker. It contains information on how to connect to the maker (a public address of `127.0.0.1:7067` by default). Additionally, it adds the deal to the set of deals available in `farcasterd` that will be later used to initiate the swap upon requests from takers.

To get a detailed explanation of `make` and more details on all possible arguments you can run `swap1-cli make --help`.

## Take a deal

Taker accepts deals and connects to maker's daemon node with the following command (run `swap2-cli take --help` for more details):

Example of taking the deal above produced by maker:

```
swap2-cli take --btc-addr tb1qt3r3t6yultt8ne88ffgvgyym0sstj4apwsz05j\
    --xmr-addr 54EYTy2HYFcAXwAbFQ3HmAis8JLNmxRdTC9DwQL7sGJd4CAUYimPxuQHYkMNg1EELNP85YqFwqraLd4ovz6UeeekFLoCKiu\
    --deal Deal:...
```

The `--btc-addr` argument is the Bitcoin destination address in case the bitcoins need to be refunded or if the swap completes (depending on the trade you're doing), the `--xmr-addr` for Monero, and finally the deal with `--deal` or `-D`.

Upon taking the deal, its deserialized content is printed and the user is asked for validation with `y` or `n`.

:mag_right: Flag of interest: `--without-validation` or `-w`, for externally validated automated setups (skip validation in cli).

---

If you want to use your full nodes and infrastructure you can change the configuration to the following.

Farcaster needs to connect to three services to do actions on-chain and track on-chain events. Needed services are: an `electrum server`, a `monero daemon`, and a `monero rpc wallet`.

You can launch all the needed services locally by running the following commands:

```sh
bitcoind -server -testnet
electrs --network testnet
monerod --stagenet
monero-wallet-rpc --stagenet --rpc-bind-port 38083\
    --disable-rpc-login\
    --trusted-daemon\
    --password "pw"\
    --wallet-dir ~/.fc_monero_wallets
```

Then start the node with:

```
farcasterd -c farcasterd.toml
```

with `farcasterd.toml` configuration file:

```toml
[syncers.testnet]
electrum_server = "tcp://localhost:60001"
monero_daemon = "http://localhost:38081"
monero_rpc_wallet = "http://localhost:38083"
```

