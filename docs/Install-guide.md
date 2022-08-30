To install the Farcaster stack on your machine from sources you need some terminal knowledge and know how to use your package manager. We provide an example for Debian/Ubuntu Linux flavors, Arch Linux and macOS, but you should be able to derive the requirements for your platform quite easily. 

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
