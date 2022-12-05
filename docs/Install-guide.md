To install the Farcaster stack on your machine from sources you need some terminal knowledge and know how to use your package manager. We provide an example for Debian/Ubuntu Linux flavors, Arch Linux and macOS, but you should be able to derive the requirements for your platform quite easily. 

## Install from sources

After instaling [Rust](https://www.rust-lang.org/tools/install) don't forget to run `source $HOME/.cargo/env` or start a new terminal session. The binaries will be installed in `$HOME/.cargo/bin`.

Before installing from sources install the dependencies needed on [`Debian`](#ubuntu--debian-1011), [`Archlinux`](#archlinux), or [`macOS`](#macos-1112) and Rust.

```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

Install from [crates.io](https://crates.io/crates/farcaster_node)

```
cargo install farcaster_node --force
```

or clone and build the project

```
git clone https://github.com/farcaster-project/farcaster-node.git && cd farcaster-node
cargo install --path . --bins --all-features --locked --force
```

### Ubuntu & Debian 10/11

Install dependencies

```
apt-get install -y git curl libssl-dev pkg-config build-essential cmake
```

### Archlinux

Install dependencies

```
pacman -Sy git base-devel cmake
```

### macOS 11/12

Install [Homebrew](https://brew.sh/) and [Rust](https://www.rust-lang.org/tools/install), then install the dependencies

```
brew install cmake
```
