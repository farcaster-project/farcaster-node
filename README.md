# Farcaster-node: Atomic swap node
:warning: THIS IS UNFINISHED, EXPERIMENTAL TECH AND YOU WILL LOSE YOUR MONEY IF YOU TRY IT ON MAINNET :warning:

## Running the node
### Clone and build the project
```
git clone https://github.com/farcaster-project/farcaster-node.git
cd farcaster-node
cargo build --all-features
```

### Launch full nodes, electrum, monero-wallet-rpc
#### Launch bitcoind
``` sh
bitcoind -server -testnet
```

#### Launch electrs (electrum server in rust)
``` sh
electrs -vvvv --network testnet
```

#### Launch monerod
``` sh
monerod --stagenet
```

#### Launch wallet-rpc
``` sh
monero-wallet-rpc --stagenet --rpc-bind-port 18083 --disable-rpc-login --trusted-daemon --password "pw" --wallet-dir ~/.monero_wallets
```

### Launch two farcaster nodes for trading

On one terminal launch the farcasterd node `node0` with `data_dir_0`
```
 ./target/debug/farcasterd -vv -d .data_dir_0 --electrum-server localhost:60001 --monero-daemon http://stagenet.melo.tools:38081 --monero-rpc-wallet http://localhost:18083
 
```
On a second terminal launch a second farcasterd node `node1` with `data_dir_1`
```
 ./target/debug/farcasterd -vv -d .data_dir_1 --electrum-server localhost:60001 --monero-daemon http://stagenet.melo.tools:38081 --monero-rpc-wallet http://localhost:18083
```

### Client

On a third terminal

Create aliases for nodes client. You will use them to make and take offers.
```
# client for node0
alias swap0-cli="./target/debug/swap-cli -d .data_dir_0" 
# client for node1
alias swap1-cli="./target/debug/swap-cli -d .data_dir_1"
```

### Make and take offer
### Make
Maker creates offer and start listening. Command used to to print a hex representation of the offer that shall be shared with Taker. Additionally it spins up the listener awaiting for connection related to this offer. params of make:
```
swap0-cli make --help
```
```
ARGS:
    <ARBITRATING_ADDR>
            bitcoin address used as destination or refund address

    <NETWORK>
            Type of offer and network to use [default: Testnet]

    <ARBITRATING_BLOCKCHAIN>
            The chosen arbitrating blockchain [default: ECDSA]

    <ACCORDANT_BLOCKCHAIN>
            The chosen accordant blockchain [default: Monero]

    <ARBITRATING_AMOUNT>
            Amount of arbitrating assets to exchanged [default: "0.15 BTC"]

    <ACCORDANT_AMOUNT>
            Amount of accordant assets to exchanged [default: "100 XMR"]

    <MAKER_ROLE>
            The future maker swap role [default: Alice] [possible values: Alice, Bob]

    <CANCEL_TIMELOCK>
            The cancel timelock parameter of the arbitrating blockchain [default: 10]

    <PUNISH_TIMELOCK>
            The punish timelock parameter of the arbitrating blockchain [default: 30]

    <FEE_STRATEGY>
            The chosen fee strategy for the arbitrating transactions [default: "2 satoshi/vByte"]

    <PUBLIC_IP_ADDR>
            public IPv4 or IPv6 address for public offer [default: 127.0.0.1]

    <BIND_IP_ADDR>
            IPv4 or IPv6 address to bind to [default: 0.0.0.0]

    <PORT>
            Port to use; defaults to the native LN port [default: 9735]

    <OVERLAY>
            Use overlay protocol (http, websocket etc) [default: tcp]
```

The `ECDSA` below is a temporary hack, but it represents `Bitcoin<ECDSA>`, as Bitcoin can take many forms:

```
swap0-cli make tb1q4gj53tuew3e6u4a32kdtle2q72su8te39dpceq Testnet ECDSA Monero "0.00001350 BTC" "0.00000001 XMR" Alice 10 30 "1 satoshi/vByte" "127.0.0.1" "0.0.0.0" 9745
```

This will produce the following hex encoded offer: 

`4643535741500100020000008080000080080046050000000000000800102700000000000004000a00000004001e000000010800010000000000000001210002904c04c85a9d027fc44f5474438a1a98fd69bf64cef96608cb7d97a3b33b25670000000000000000000000000000000000000000000000000000000000007f000001261200`

This public offer should be shared by maker with taker. It also contains information on how to connect to maker.

Additionally, it adds the public offers to the set of public offers in farcasterd that will be later used to initiate the swap upon takers message
 
### Take
Taker accepts offer and connects to Maker's daemon

arguments of `take`
```
    <BITCOIN_ADDRESS>
            bitcoin address used as destination or refund address

    <PUBLIC_OFFER>
            Hex encoded offer
```

flag of interest: `--without-validation` or `-w`, for externally validated automated setups


Example of taking the offer above produced by maker. 
```
swap1-cli take tb1qt3r3t6yultt8ne88ffgvgyym0sstj4apwsz05j 4643535741500100020000008080000080080046050000000000000800102700000000000004000a00000004001e000000010800010000000000000001210002414dbe27712feb696a5f9f7d86eb37cb1317acc49a8a78e051dfe0c88efdff500000000000000000000000000000000000000000000000000000000000007f000001261200
```

## Remote client use
Example from using other URLs supported by crate `internet2` `node_addr.rs`, besides the default inter process communication. 

### Farcasterd (Server)
Here the node public key of farcasterd is derived from its secretkey included in `key.dat` file. You can launch the node first WITHOUT `-x` and `-m` to retrieve the printed pubkey, and later again on the same `-d data_dir` passing Ctl bus `-x` and Msg bus `-m` arguments:
``` sh
./target/debug/farcasterd -vvvv -x "lnpz://02c21ee2baf368b389059d4d2b75b734526aec8cc629481d981d4f628844f2f114@127.0.0.1:9981/?api=esb" -m "lnpz://02c21ee2baf368b389059d4d2b75b734526aec8cc629481d981d4f628844f2f114@127.0.0.1:9982/?api=esb" -d .data_dir_1
```


### Client
And the following client can instruct the above `farcasterd` to make a offer as follows:
``` sh
./target/debug/swap-cli -x "lnpz://02c21ee2baf368b389059d4d2b75b734526aec8cc629481d981d4f628844f2f114@127.0.0.1:9981/?api=esb" -m "lnpz://02c21ee2baf368b389059d4d2b75b734526aec8cc629481d981d4f628844f2f114@127.0.0.1:9982/?api=esb" make
```

## Build and usage

### Local

To compile the node, please install [cargo](https://doc.rust-lang.org/cargo/),
then run the following commands:

```bash
sudo apt install -y libsqlite3-dev libssl-dev libzmq3-dev pkg-config
cargo install --path . --bins --all-features --locked
farcasterd -vv\
    --electrum-server {ip:port}\
    --monero-daemon http://{ip:port}\
    --monero-rpc-wallet http://{ip:port}
```

### In docker

You can use the docker container produced in the CI with:

```
docker run --rm -t -p 9735:9735 -p 9981:9981 --name farcaster_node ghcr.io/farcaster-project/farcaster-node/farcasterd:latest
```

or build the node container with `docker build -t farcasterd:latest .` inside the project folder and then run:

```
docker run --rm -t -p 9735:9735 -p 9981:9981 --name farcaster_node farcasterd:latest
```

The container will be removed after execution (`--rm`), allocate a pseudo-TTY (`-t`), and publish exposed ports `9735` and `9981` on the host.

It is then possible to command `farcasterd` with `swap-cli -x "lnpz://127.0.0.1:9981/?api=esb" info` from the host.

Stop the container with `docker stop farcaster_node` (ctrl+c does not work yet).

:warning: this exposes the control bus on the host, only intended for debug or on a trusted network.

## Ways of communication

IRC channels on Libera.chat \#monero-swap, Bitcoin-Monero cross-chain atomic swaps research and development.
