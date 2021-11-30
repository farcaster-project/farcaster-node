[![GitHub Workflow Status](https://img.shields.io/github/workflow/status/farcaster-project/farcaster-node/Build%20binaries)](https://github.com/farcaster-project/farcaster-node/actions/workflows/binaries.yml)
[![unsafe forbidden](https://img.shields.io/badge/unsafe-forbidden-success.svg)](https://github.com/rust-secure-code/safety-dance)
[![Crates.io](https://img.shields.io/crates/v/farcaster_node.svg)](https://crates.io/crates/farcaster_node)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![MSRV](https://img.shields.io/badge/MSRV-1.55.0-blue)](https://blog.rust-lang.org/2021/09/09/Rust-1.55.0.html)

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
monero-wallet-rpc --stagenet --rpc-bind-port 38083\
    --disable-rpc-login\
    --trusted-daemon\
    --password "pw"\
    --wallet-dir ~/.fc_monero_wallets
```

Then start the node with:

```
farcasterd -vv -c farcasterd.toml
```

with `farcasterd.toml` configuration file:

```toml
[syncers.testnet]
electrum_server = "tcp://localhost:60001"
monero_daemon = "http://localhost:38081"
monero_rpc_wallet = "http://localhost:38083"
```

#### Connect a client

Once `farcasterd` is up & running you can issue commands to control its actions with a client. For the time being, only one client is provided within this repo: `swap-cli`.

If you launched `farcasterd` with the default paramters (the `--data-dir` argument or `-d`), `swap-cli` will be able to connect to `farcasterd` without further configuration. You can get informations about the node with (this require `swap-cli` to be installed on your host):

```
swap-cli info
```

Run `help` command for more details about available commands.

### With docker

You can follow the documentation on [how to use `farcasterd` with Docker](./doc/docker-stack.md) and [how to connect a remote client to `farcasterd`](./doc/docker-stack.md#connect-a-client).

Running `monero-wallet-rpc` is also possible with Docker, see [run `monero rpc wallet`](./doc/docker-stack.md#run-monero-rpc-wallet) for more details.

### Configuration

`farcasterd` can be configured through a toml file located by default at `~/.farcaster/farcasterd.toml`, if no file is found `farcasterd` is launched with some default values. You can see an example [here](./farcasterd.toml).

**Syncers**

Configures daemon's URLs where to connect for the three possible networks: _mainnet_, _testnet_, _local_:

```toml
[syncers.{network}]
electrum_server = ""
monero_daemon = ""
monero_rpc_wallet = ""
```

:mag_right: The default config for _local_ network is set to null.

#### :bulb: Use public infrastructure

To help quickly test and avoid running the entire infrastructure on your machine you can make use of public nodes. Following is a non-exhaustive list of public nodes.

Only blockchain daemons and electrum servers are listed, you should always run your own `monero rpc wallet`.

**Mainnet**

| daemon            | value                                                |
| ----------------- | ---------------------------------------------------- |
| `electrum server` | `ssl://blockstream.info:700` **(default)**           |
| `monero daemon`   | `http://node.melo.tools:18081`                       |
| `monero daemon`   | `http://node.monerooutreach.org:18081` **(default)** |

**Testnet/Stagenet**

| daemon            | value                                            |
| ----------------- | ------------------------------------------------ |
| `electrum server` | `ssl://blockstream.info:993` **(default)**       |
| `monero daemon`   | `http://stagenet.melo.tools:38081` **(default)** |

## Usage

:rotating_light: **The following section focus on how to use the Farcaster Node to propose and run atomic swaps. Keep in mind that this software remains experimental and should not be used on mainnet or with any valuable assets.**

When `farcasterd` is up & running and `swap-cli` is configured to connect and control it, you can make offers and/or take offers. An offer encapsulate informations about a trade of Bitcoin and Monero. One will make :hammer: an offer, e.g. a market maker, and one will try to take :moneybag: the offer. Below are the commands to use to either `make` an offer or `take` one.

### :hammer: Make an offer

When listening for other peers to connect, e.g. for executing a swap, a `peerd` instance is spawned and binds to the specified `address:port` in arguments, counterparty `farcasterd` can then launch its own `peerd` that connects to the listening `peerd`, the communication is then established between two nodes.

:mag_right: This requires for the time being some notions about the network topology the maker node is running in, this requirement will be lifted off later when integrating Tor by default.

To create an offer and spawn a listening `peerd` accepting incoming connections, run the following command:

```
swap-cli make --arb-addr tb1q935eq5fl2a3ajpqp0e3d7z36g7vctcgv05f5lf\
    --acc-addr 54EYTy2HYFcAXwAbFQ3HmAis8JLNmxRdTC9DwQL7sGJd4CAUYimPxuQHYkMNg1EELNP85YqFwqraLd4ovz6UeeekFLoCKiu\
    --arb-amount "0.0000135 BTC" --acc-amount "0.001 XMR"\
    --maker-role Bob\
    --public-ip-addr 1.2.3.4 --port 9735
```

Network and assets by default are Bitcoin and Monero on testnet. The first arguments `--arb-addr` and `--acc-addr` are the Bitcoin and Monero addresses used to get the bitcoins and moneros as a refund or when the swap completes depending on the role. They are followed by the amounts exchanged.

The role for the maker is specified in the offer with `--maker-role`. `Alice` sells moneros for bitcoins, `Bob` sells bitcoins for moneros. Timelock parameters are set by default to **4** and **5** for cancel and punish and the transaction fee that must be applied is by default **1 satoshi per vByte**.

Here the maker will send bitcoins and will receive moneros in his `54EYTy2HYFcAXwAbFQ3HmAis8JLNmxRdTC9DwQL7sGJd4CAUYimPxuQHYkMNg1EELNP85YqFwqraLd4ovz6UeeekFLoCKiu` address if the swap is successful, 4 and 5 blocks are used for the timelocks and 1 satoshi per virtual byte must be used for the Bitcoin transaction fee (default values).

Then the last two arguments in this example are: `--public-ip-addr` (default to `127.0.0.1`) and `--port` to be explicit in this example (default to `9735`). We don't specify `--bind-ip-addr` and leave its default to `0.0.0.0`,

:mag_right: To be able for a taker to connect and take the offer the `public-ip-addr:port` must be accessible and answered by the `peerd` binded to `bind-id-address:port`.

**The public offer result**

The command will ouput an encoded **public offer** that must be shared to anyone susceptible to take it and your `farcasterd` will register this public offer in its list, waiting for someone to connect and take it.

Follow your `farcasterd` log (**with a log level set at `-vv`**) and fund the swap with the bitcoins or moneros when it asks so, at the end you should receive the counterparty assets.

### :moneybag: Take the offer

Taking a public offer is a much simpler process, all you need is a running node (doesn't require to know your network topology), an encoded public offer, a Bitcoin address and a Monero address to receive assets, again as a refund or as a payment depending on your swap role and if the swap completes.

```
swap-cli take --arb-addr tb1qmcku4ht3tq53tvdl5hj03rajpdkdatd4w4mswx\
    --acc-addr 54EYTy2HYFcAXwAbFQ3HmAis8JLNmxRdTC9DwQL7sGJd4CAUYimPxuQHYkMNg1EELNP85YqFwqraLd4ovz6UeeekFLoCKiu\
    --offer {offer}
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
