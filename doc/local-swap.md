# Run a swap locally

This document describe how to test a swap running both participant locally.

You'll need to build and install the node locally as described [here](../README.md#locally).

We'll create two node instances and will make them running a swap, for that make sure to have access to the three services: an `electrum server`, a `monero daemon`, and a `monero rpc wallet`.

## Run `monero rpc wallet` with Docker

:warning: You should run two containers, one for each `farcasterd` services, choose different ports and modify `--rpc-bind-port` and `-p` values accordingly.

See [run `monero rpc wallet`](./docker-stack.md#run-monero-rpc-wallet) with Docker for more details.

## Launch `farcasterd` services

Launch two `farcasterd` services with different data directories (`.node01` and `.node02`):

```
farcasterd -vv\
    --data-dir {.node01|.node02}\
    --electrum-server ssl://blockstream.info:993\
    --monero-daemon http://stagenet.melo.tools:38081\
    --monero-rpc-wallet http://localhost:{38083|38084}
```

You can use other public daemons, see [:bulb: Use public infrastructure](../README.md#bulb-use-public-infrastructure) for more details.

## Configure `swap-cli` clients

For simplicity you can create two aliases for the `swap-cli`, you will use them to make and take offers:

```
alias swap1-cli="swap-cli -d .node01"
alias swap2-cli="swap-cli -d .node02"
```

## Access to assets

You need to have access to Bitcoin (testnet) and Monero (stagenet) test coins, use your wallet of choice. You just need a way to fund the swap in bitcoins and moneros.

## Make the offer

Maker creates offer and start listening. Command used to print a serialized representation of the offer that shall be shared with taker. Additionally it spins up the listener awaiting for connection related to this offer (binded on `0.0.0.0:9735` with an offer public address of `127.0.0.1:9735`).

```
swap1-cli make tb1q4gj53tuew3e6u4a32kdtle2q72su8te39dpceq\
    54EYTy2HYFcAXwAbFQ3HmAis8JLNmxRdTC9DwQL7sGJd4CAUYimPxuQHYkMNg1EELNP85YqFwqraLd4ovz6UeeekFLoCKiu\
    Testnet ECDSA Monero\
    "0.00001350 BTC" "0.00000001 XMR"\
    Alice 4 5 "1 satoshi/vByte"\
    "127.0.0.1" "0.0.0.0" 9735
```

:mag_right: The `ECDSA` above is a temporary hack, but it represents `Bitcoin<ECDSA>`, as Bitcoin can take many forms.

This will produce the following encoded offer:

`Offer:Cke4ftrP5A71LQM2fvVdFMNR4gmBqNCsR11111uMM4pF11111112Lvo11111TB9zym113GTvtvqfD1111112g5B8NNdQ79CnoUCEtZVSK1pYzGhNQeM28QBZNSKAZ5sJk8WA11111111111111111111111111111111111111111AfZ113XRBtzfJQ4h`

This public offer should be shared by maker with taker. It also contains information on how to connect to maker. Additionally, it adds the public offers to the set of public offers in `farcasterd` that will be later used to initiate the swap upon takers message.

To get a detailed explaination of make parameters run `swap1-cli make --help`.

## Take an offer

Taker accepts offer and connects to maker's daemon node with the following command (run `swap2-cli take --help` for more details):

Example of taking the offer above produced by maker:

```
swap2-cli take tb1qt3r3t6yultt8ne88ffgvgyym0sstj4apwsz05j\
    54EYTy2HYFcAXwAbFQ3HmAis8JLNmxRdTC9DwQL7sGJd4CAUYimPxuQHYkMNg1EELNP85YqFwqraLd4ovz6UeeekFLoCKiu\
    Offer:Cke4ftrP5A71LQM2fvVdFMNR4gmBqNCsR11111uMM4pF11111112Lvo11111TB9zym113GTvtvqfD1111112g5B8NNdQ79CnoUCEtZVSK1pYzGhNQeM28QBZNSKAZ5sJk8WA11111111111111111111111111111111111111111AfZ113XRBtzfJQ4h
```

The first argument is the Bitcoin destination address in case the bitcoins needs to be refunded or if the swap completes (depending on the swap role).

:mag_right: Flag of interest: `--without-validation` or `-w`, for externally validated automated setups (skip validation in cli).
