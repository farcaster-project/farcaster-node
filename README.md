[![GitHub Workflow Status](https://img.shields.io/github/workflow/status/farcaster-project/farcaster-node/Build%20binaries)](https://github.com/farcaster-project/farcaster-node/actions/workflows/binaries.yml)
[![Crates.io](https://img.shields.io/crates/v/farcaster_node.svg)](https://crates.io/crates/farcaster_node)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![MSRV](https://img.shields.io/badge/MSRV-1.59.0-blue)](https://blog.rust-lang.org/2022/02/24/Rust-1.59.0.html)

# Farcaster: cross-chain atomic swaps

**This work is still evolving, use it on mainnet at your own risk!**

The **Farcaster Node** is _a collection of microservices for running cross-chain atomic swaps_. Currently, the node is focused on Bitcoin-Monero atomic swaps, but it is designed to be flexible and integrate new crypto-pairs in the future.

Microservices currently implemented:

- **farcasterd** (1 instance): the swap manager, it is aware of every initiated swap and interconnects all the other microservices, launches and kills other microservices, and exposes an API for the swap-cli client
- **swapd** (1 instance per swap): control centre for an individual swap -- keeps track of the swap's state as it runs the protocol's state machine, and orchestrates the swap with peerd for communicating with swap counterparty, walletd for signing, and syncers for blockchain interactions.
- **walletd** (1 instance): where secret keys live, where transactions are signed, and coordinates with swapd.
- **swap-cli**: stateless terminal client (=executes a single command and terminates) that commands farcasterd, for taking or making deals, for example.
- **peerd** (1 instance per peer connection): handles the connection to an individual peer.
- **syncerd** (1 instance per blockchain, i.e. one for monero and one for bitcoin): interface for getting updates of the blockchain and for broadcasting transactions.
- **databased** (1 instance): interface for storing data persistently across restart.
- **grpcd** (1 instance): interface for exposing node interfaces as a gRPC endpoint.

Farcaster Node is build on atomic swap primitives described in the [RFCs](https://github.com/farcaster-project/RFCs) and implemented in [Farcaster Core](https://github.com/farcaster-project/farcaster-core).

## Documentation

Checkout Farcaster documentation in the [wiki](https://github.com/farcaster-project/farcaster-node/wiki) or in the `docs/` folder.

## Releases and Changelog

See [CHANGELOG.md](CHANGELOG.md) and [RELEASING.md](RELEASING.md).

## About

This work is part of the Farcaster cross-chain atomic swap project, see [Farcaster Project](https://github.com/farcaster-project), and is based on LNP/BP work, this project is a fork from [LNP-BP/lnp-node](https://github.com/LNP-BP/lnp-node) since [acbb4c](https://github.com/farcaster-project/farcaster-node/commit/acbb4c467695dc3d1c02b88be97e9a6e2d434435).

## Licensing

The code in this project is licensed under the [MIT License](LICENSE).

## Ways of communication

IRC channels on Libera.chat \#monero-swap, Bitcoin-Monero cross-chain atomic swaps research and development.
