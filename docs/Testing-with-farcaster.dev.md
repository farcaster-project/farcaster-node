**[Farcaster.dev](https://farcaster.dev) is a website for automating Farcaster offers creation and publication (currently only testnet trades).**

In this guide we will go through all the necessary steps to install and setup a node on your machine, choose and take a public offer listed there.

## Requirements

- macOS or Linux
- Docker or access to a running Monero Wallet RPC instance
- Access and control over some testnet bitcoin or stagenet monero
- Access and control over a bitcoin testnet wallet and a monero stagenet wallet

### Testnet Wallets

Get the Monero wallet cli [here](https://www.getmonero.org/downloads/#cli). Run it in stagenet mode with `./monero-wallet-cli --stagenet`. For Bitcoin testnet we recommend the [Electrum wallet](https://electrum.org/#download) - this can be run in testnet mode by passing in `--testnet`. 

## Installing and running Farcaster 

[Using Docker](./Using-Docker) can be avoided if preferred. You can choose to run everything inside Docker or run everything outside, for that checkout the [Install guide](Install-guide).

In this guide we suggest & assume the following setup:

- Farcaster node & swap cli are installed on your machine
- Monero Wallet RPC runs inside Docker as a temporary container (removed after use)

### Install Farcaster from sources

You find all the necessary steps to install farcaster from sources [here](./Install-guide#install-from-sources).

Now you should be able to run in your shell `farcasterd --help` and `swap-cli --help` and receive answers. If this is not the case, check that the binaries compiled successfully and their location is in your `$PATH` (see `cargo install` documentation).

### Running the node

We first have to launch the Monero Wallet RPC daemon, then we'll start Farcaster so it can connect to it and other Bitcoin and Monero public nodes.

Start the Monero Wallet RPC daemon container
```
docker run --rm -p 38083 ghcr.io/farcaster-project/containers/monero-wallet-rpc:latest\
    /usr/bin/monero-wallet-rpc --stagenet\
    --disable-rpc-login --wallet-dir wallets\
    --daemon-host stagenet.community.rino.io:38081\
    --rpc-bind-ip 0.0.0.0 --rpc-bind-port 38083\
    --confirm-external-bind
```

Start Farcaster node in a second terminal
```
farcasterd
```

Use the cli to query `farcasterd`
```
swap-cli info
```

If you manage to go through all the steps above and you have a running node, congrats! you should now be able to try taking an offer on [farcaster.dev](https://farcaster.dev).

## Taking offer on [farcaster.dev](https://farcaster.dev)

First steps:
- Get a destination Bitcoin address
- Get a destination Monero address

No matter what type of trade you choose to do, BTC->XMR or XMR->BTC, you need an address on each chain:
1. a success path address to receive traded funds and
2. a failure path address in case a refund is initiated

When you have these two addresses go to the website and choose an offer that correspond to the trade you want to take. Then use `swap-cli` to start trading
```
swap-cli --btc-addr {...} --xmr-addr {...} -o Offer:...
```

You can now follow what's happening on the `farcasterd` logs in your shell and use `swap-cli` to query more details. Use
```
swap-cli --help
```
to see what subcommands are available. You might find the following ones useful in particular:
- `list-swaps`
- `needs-funding <btc|xmr>`
- `progress <swap>`
