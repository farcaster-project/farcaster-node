# Installation Guide

We provide two methods to install Farcater: compiling the software from [sources](#install-from-sources) or using [containers](#with-docker-and-docker-compose).

To install Farcaster on your machine from sources you need some terminal knowledge and know how to use your package manager.

## Install from sources

After instaling [Rust](https://www.rust-lang.org/tools/install) don't forget to run `source $HOME/.cargo/env` or start a new terminal session. The binaries will be installed in `$HOME/.cargo/bin`.

### Ubuntu & Debian 10/11

To install Farcaster on Debian Buster or Bullseye run the following commands.

Install dependencies

```
apt-get update -y
apt-get install -y git curl libssl-dev pkg-config build-essential cmake
```

Install Rust

```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

Clone and build the project

```
git clone https://github.com/farcaster-project/farcaster-node.git && cd farcaster-node
cargo install --path . --bins --all-features --locked
```

### Archlinux

Install Rust

```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

Install dependencies

```
pacman -Syy && pacman -Syu
pacman -Sy git base-devel cmake
```

Clone and build the project

```
git clone https://github.com/farcaster-project/farcaster-node.git && cd farcaster-node
cargo install --path . --bins --all-features --locked
```

### macOS 11/12

Install [Homebrew](https://brew.sh/) and [Rust](https://www.rust-lang.org/tools/install), then install the dependencies

```
brew install cmake
```

Clone and build the project

```
git clone https://github.com/farcaster-project/farcaster-node.git && cd farcaster-node
cargo install --path . --bins --all-features --locked
```

## With Docker and Docker Compose

You can use the Docker image [`farcasterd`](https://github.com/farcaster-project/farcaster-node/pkgs/container/farcaster-node%2Ffarcasterd) and images from the [`containers`](https://github.com/orgs/farcaster-project/packages?repo_name=containers) repo.

The easiet way is to use Docker compose to run both the node and the Monero wallet. Copy and paste the following in a `docker-compose.yml` file and run `docker-compose up -d`

```yaml
version: "3.9"
services:
  farcasterd:
    image: "ghcr.io/farcaster-project/farcaster-node/farcasterd:latest"
    ports:
      - "9735:9735"
      - "9981:9981"
    command: "-vv"
    depends_on:
      - "walletrpc"
  walletrpc:
    image: "ghcr.io/farcaster-project/containers/monero-wallet-rpc:0.17.2.3"
    command: "/usr/bin/monero-wallet-rpc --stagenet --disable-rpc-login --wallet-dir wallets --daemon-host stagenet.melo.tools:38081 --rpc-bind-ip 0.0.0.0 --rpc-bind-port 38083 --confirm-external-bind"
    ports:
      - "38083:38083"
```

