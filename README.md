[![GitHub Workflow Status](https://img.shields.io/github/workflow/status/farcaster-project/farcaster-node/Build%20binaries)](https://github.com/farcaster-project/farcaster-node/actions/workflows/binaries.yml)
[![unsafe forbidden](https://img.shields.io/badge/unsafe-forbidden-success.svg)](https://github.com/rust-secure-code/safety-dance)
[![Crates.io](https://img.shields.io/crates/v/farcaster_node.svg)](https://crates.io/crates/farcaster_node)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![MSRV](https://img.shields.io/badge/MSRV-1.54.0-blue)](https://blog.rust-lang.org/2021/07/29/Rust-1.54.0.html)

# Farcaster: cross-chain atomic swaps

:warning: **THIS IS UNFINISHED, EXPERIMENTAL TECH AND YOU WILL LOSE YOUR MONEY IF YOU TRY IT ON MAINNET**

This project is called the **Farcaster Node**, it is _a collection of microservices for running cross-chain atomic swaps_. Currently the node is focused on Bitcoin-Monero atomic swaps, but is designed to be flexible and integrate new crypto-pairs in the future.

Farcaster Node is build on atomic swap primitives described in the [RFCs](https://github.com/farcaster-project/RFCs) and implemented in [Farcaster Core](https://github.com/farcaster-project/farcaster-core).

:information_source: This work is based on LNP/BP work, this project is a fork from [LNP-BP/lnp-node](https://github.com/LNP-BP/lnp-node) since [acbb4c](https://github.com/farcaster-project/farcaster-node/commit/acbb4c467695dc3d1c02b88be97e9a6e2d434435).

## Build and run

The following section explain how to build and run Farcaster node locally or within containers through Docker and Docker compopse.

### Locally

To compile the node, please install [cargo](https://doc.rust-lang.org/cargo/) (the Rust _package manager_), then run the following commands:

```bash
sudo apt install -y libsqlite3-dev libssl-dev libzmq3-dev pkg-config build-essential cmake
git clone https://github.com/farcaster-project/farcaster-node.git && cd farcaster-node
cargo install --path . --bins --all-features --locked
```

Farcaster needs to connect to tree services to do actions on-chain and track on-chain events. Needed services are: an `electrum server`, a `monero daemon`, and a `monero rpc wallet`.

You can install and launch all the needed services locally by running the following commands:

```sh
bitcoind -server -testnet
electrs --network testnet
monerod --stagenet
monero-wallet-rpc --stagenet --rpc-bind-port 18083\
    --disable-rpc-login\
    --trusted-daemon\
    --password "pw"\
    --wallet-dir ~/.fc_monero_wallets
```

Then start the node with:

```
farcasterd -vv\
    --electrum-server {ip:port}\
    --monero-daemon http://{ip:port}\
    --monero-rpc-wallet http://{ip:port}
```

#### Connect a client

Once `farcasterd` is up & running you can issue commands to control its actions with a client. For the time being, only one client is provided within this repo: `swap-cli`.

If you launched `farcasterd` with the default paramters (the `--data-dir` argument), `swap-cli` will be able to connect to `farcasterd` without further configuration. You can get informations about the node with (this require `swap-cli` to be installed on your host):

```
swap-cli info
```

Run `help` command for more details about available commands.

### With docker

You can follow the documentation on [how to use `farcasterd` with Docker](./doc/docker-stack.md) and [how to connect a remote client to `farcasterd`](./doc/docker-stack.md#connect-a-client).

## Usage

:rotating_light: **The following section focus on how to use the Farcaster Node to propose and run atomic swaps. Keep in mind that this software remains experimental and should not be used on mainnet or with any valuable assets.**

When `farcasterd` is up & running and `swap-cli` is configured to connect and control it, you can make offers and/or take offers. An offer encapsulate informations about a trade of Bitcoin and Monero. One will make :hammer: an offer, e.g. a market maker, and one will try to take :moneybag: the offer. Below are the commands to use to either `make` an offer or `take` one.

### :hammer: Make an offer

When listening for other peers to connect, e.g. for executing a swap, a `peerd` instance is spawned and binds to the specified `address:port` in arguments, counterparty `farcasterd` can then launch its own `peerd` that connects to the listening `peerd`, the communication is then established between two nodes.

:mag_right: This requires for the time being some notions about the network topology the maker node is running in, this requirement will be lifted off later when integrating Tor by default.

To create an offer and spawn a listening `peerd` accepting incoming connections, run the following command:

```
swap-cli make tb1q935eq5fl2a3ajpqp0e3d7z36g7vctcgv05f5lf\
    Testnet ECDSA Monero\
    "0.00001350 BTC" "0.00000001 XMR"\
    Alice 4 6 "1 satoshi/vByte"\
    1.2.3.4\
    0.0.0.0\
    9735
```

The first argument is the Bitcoin address used to get the bitcoins (as a refund or when the swap completes depending on the role).

Then the network and assets are specified with the amounts. The `ECDSA` below is a temporary hack, but it represents `Bitcoin<ECDSA>`, as Bitcoin can take many forms.

The role for the maker is specified in the offer: `Alice`, sells moneros for bitcoins, or `Bob`, sells bitcoins for moneros, with the timelock parameters for cancel and punish and the transaction fee that must be applied. Here the maker will send moneros and will receive bitcoin in his `tb1q935eq5fl2a3ajpqp0e3d7z36g7vctcgv05f5lf` address if the swap is successful, 4 and 5 blocks are used for the timelocks and 1 satoshi per virtual byte must be used for the Bitcoin transaction fee.

Then the last three arguments in this example are: `public_ip_addr`, `bind_id_address` (default to `0.0.0.0`), and `port` (default to `9735`).

:mag_right: To be able for a taker to connect and take the offer the `public_ip_addr:port` must be accessible and answered by the `peerd` binded to `bind_id_address:port`.

**The public offer result**

The command will ouput a hex encoded **public offer** that must be shared to anyone susceptible to take it and your `farcasterd` will register this public offer in its list, waiting for someone to connect and take it.

Follow your `farcasterd` log (**with a log level set at `-vv`**) and fund the swap with the bitcoins or moneros when it asks so, at the end you should receive the counterparty assets.

### :moneybag: Take the offer

Taking a public offer is a much simpler process, all you need is a running node (doesn't require to know your network topology), an hex encoded public offer, and a bitcoin address to receive bitcoins (again, as a refund or if the swap completes depending on your swap role).

```
swap-cli take tb1qmcku4ht3tq53tvdl5hj03rajpdkdatd4w4mswx {hex encoded offer}
```

The cli will ask you to validate the offer informations (amounts, assets, etc.), you can use the flag of interest `--without-validation` or `-w` for externally validated automated setups.

Then follow your `farcasterd` log (**with a log level set at `-vv`**) and fund the swap with the bitcoins or moneros when it asks so, at the end you should receive the counterparty assets.

### Run a swap locally

If you want to test a swap with yourself locally you can follow the instructions [here](./doc/local-swap.md).

## Releases and Changelog

See [CHANGELOG.md](CHANGELOG.md) and [RELEASING.md](RELEASING.md).

## About

This work is part of the Farcaster cross-chain atomic swap project, see [Farcaster Project](https://github.com/farcaster-project).

## Licensing

The code in this project is licensed under the [MIT License](LICENSE).

## Ways of communication

IRC channels on Libera.chat \#monero-swap, Bitcoin-Monero cross-chain atomic swaps research and development.
