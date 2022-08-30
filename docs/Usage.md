:rotating_light: **The following section focus on how to use the Farcaster Node to propose and run atomic swaps. Keep in mind that this software remains new and should be used at your own risks.**

When `farcasterd` is up & running and `swap-cli` is configured to connect and control it, you can make offers and/or take offers. An offer encapsulates informations about a trade of Bitcoin and Monero. One will make :hammer: an offer, e.g. a market maker, and one will try to take :moneybag: the offer. Below are the commands to use to either `make` an offer or `take` one.

## :moneybag: Take the offer

Taking a public offer is a simple process: all you need is a running node (doesn't require to know your network topology), an encoded public offer, a Bitcoin address and a Monero address to receive assets or refund, depending on your swap role and if the swap completes.

```
swap-cli take --btc-addr {your_btc_address, e.g. tb1qmcku4ht3tq53tvdl5hj03rajpdkdatd4w4mswx}\
    --xmr-addr {your_xmr_address, e.g. 54EYTy2HYFcAXwAbFQ3HmAis8JLNmxRdTC9DwQL7sGJd4CAUYimPxuQHYkMNg1EELNP85YqFwqraLd4ovz6UeeekFLoCKiu}\
    --offer {offer}
```

The cli will ask you to validate the offer's specifics (amounts, assets, etc.).

:mag_right: You can use the flag of interest `--without-validation` or `-w` for externally validated automated setups.

Then follow your `farcasterd` logs and fund the swap with the bitcoins or moneroj when it asks so. At the end of the swap, you should receive the counter-party's assets.

## :hammer: Make an offer

If you want to propose a trade to someone you have to make an offer. After making an offer, the maker starts listening for other peers to connect and take that offer -- and hopefully execute a swap successfully.

A `peerd` instance is spawned by the maker and binds to the specified `address:port`. The taker's `farcasterd` then launches its own `peerd` that connects to the makers `peerd`. The communication is then established between two nodes, and they can pass encrypted peer messages and execute the swap.

:mag_right: This requires for the time being some notions about the network topology the maker node is running in; this requirement will be removed once we're integrating Tor by default.

If you are the maker, to make an offer and spawn a listener awaiting for takers to take that offer, run the following command:

```
swap-cli make --btc-addr tb1q935eq5fl2a3ajpqp0e3d7z36g7vctcgv05f5lf\
    --xmr-addr 54EYTy2HYFcAXwAbFQ3HmAis8JLNmxRdTC9DwQL7sGJd4CAUYimPxuQHYkMNg1EELNP85YqFwqraLd4ovz6UeeekFLoCKiu\
    --btc-amount "0.0000135 BTC" --xmr-amount "0.001 XMR"\
    --network testnet --arb-blockchain bitcoin --acc-blockchain monero\
    --maker-role Bob --cancel-timelock 4 --punish-timelock 5 --fee-strategy "1 satoshi/vByte"\
    --public-ip-addr 1.2.3.4 --bind-ip-addr 0.0.0.0 --port 9735
```

:mag_right: Again, you must own the addresses given in arguments!

The `btc-addr` and `xmr-addr` are your external wallet addresses, where the coins will end up upon successful or failure cases. They are followed by the amounts exchanged. Assets and networks defaults to Bitcoin and Monero on testnet (Bitcoin testnet3, Monero stagenet).

The role for the maker is specified in the offer with `--maker-role`. `Alice` sells moneroj for bitcoins, `Bob` sells bitcoins for moneroj. Timelock parameters are set to **4** and **5** (testnet values, for mainnet this should be bigger) for cancel and punish and the transaction fee that must be applied is **1 satoshi per vByte**.

Here the maker will send bitcoins and will receive moneroj in her `54EYTy2HYFcAXwAbFQ3HmAis8JLNmxRdTC9DwQL7sGJd4CAUYimPxuQHYkMNg1EELNP85YqFwqraLd4ovz6UeeekFLoCKiu` address if the swap is successful.

`--public-ip-addr` (default to `127.0.0.1`) and `--port` (default to `9735`) are used in the public offer for the taker to connect. `--bind-ip-addr` allows to bind the listening peerd to `0.0.0.0`.

:mag_right: To enable a taker to connect and take the offer the `public-ip-addr:port` must be accessible and answered by the `peerd` bound to `bind-id-address:port`.

So maker must make sure her router allows external connections to that port.

**The public offer result**

The `make` command will output an encoded **public offer** that can be shared with potential takers. As a maker, your `farcasterd` registers this public offer, and waits for someone to connect through `peerd` and take the offer. A taker in her turn takes the offer and initiates a swap with the maker.

Follow your `farcasterd` logs (**you can fine tune your log with `RUST_LOG` environment variable, e.g. `RUST_LOG="farcaster_node=debug,microservices=debug"`**) and fund the swap with the bitcoins or moneroj when the log asks for this. At the end coins are swapped successfully, or - less ideally - refunded. We currently offer no manual cancel functionality. We offer progress through `swap-cli progress {swapid}`. To list the swapids of the running swaps, use `swap-cli ls`.

## Manage public offers

You can list registered public offer in your node with the command:
```
swap-cli list-offers [--select <SELECT>]
```

The selector can have a value of: `open`, `inprogress`, `ended`, `all`

To remove an offer you can use the command:
```
swap-cli revoke-offer <PUBLIC_OFFER>
```

## List ongoing swaps

```
swap-cli list-swaps
```

It is possible to abort a swap if not already funded:
```
swap-cli abort-swap <SWAP_ID>
```

## Use checkpoints

When a swap is running checkpoints are created and stored in a database. You can list check-pointed swaps with:
```
swap-cli list-checkpoints
```

If you restart you node and wants to restore the last checkpoint of a swap run:
```
swap-cli restore-checkpoint <SWAP_ID>
```