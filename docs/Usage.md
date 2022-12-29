:rotating_light: **The following section focus on how to use the Farcaster Node to propose and run atomic swaps. Keep in mind that this software remains new and should be used at your own risks.**

When `farcasterd` is up & running and `swap-cli` is configured to connect and control it, you can make deals and/or take deals. A deal encapsulates information about a trade of Bitcoin and Monero. One will make :hammer: a deal, e.g. a market maker, and one will try to take :moneybag: the deal. Below are the commands to use to either `make` a deal or `take` one.

## Take the deal

Taking a deal is a simple process: all you need is a running node (doesn't require to know your network topology), an encoded deal, a Bitcoin address and a Monero address to receive assets or refund, depending on the trade and if the swap completes.

```
swap-cli take --btc-addr {your_btc_address, e.g. tb1qmcku4ht3tq53tvdl5hj03rajpdkdatd4w4mswx}\
    --xmr-addr {your_xmr_address, e.g. 54EYTy2HYFcAXwAbFQ3HmAis8JLNmxRdTC9DwQL7sGJd4CAUYimPxuQHYkMNg1EELNP85YqFwqraLd4ovz6UeeekFLoCKiu}\
    --deal {deal}
```

The cli will ask you to validate the deal's specifics (amounts, assets, etc.).

:mag_right: You can use the flag of interest `--without-validation` or `-w` for externally validated automated setups.

Then follow your `farcasterd` logs and fund the swap with the bitcoins or moneroj when it asks so. At the end of the swap, you should receive the counter-party's assets.

## Make a deal

If you want to propose a trade to someone you have to make a deal. After making a deal, the maker starts listening for other peers to connect and take that deal -- and hopefully execute a swap successfully.

:mag_right: A deal is designed to be used only once, if you want to trade with two counter-parties you must create two separate deals.

A `peerd` instance is spawned by the maker and binds to the `address:port` specified in the config file. The taker's `farcasterd` then launches its own `peerd` that connects to the makers `peerd`. The communication is then established between two nodes, and they can pass encrypted peer messages and execute the swap.

:mag_right: This requires some notions about the network topology the maker node is running in.

If you are the maker, to make a deal and spawn a listener awaiting for takers to take that deal, run the following command:

```
swap-cli make --btc-addr tb1q935eq5fl2a3ajpqp0e3d7z36g7vctcgv05f5lf\
    --xmr-addr 54EYTy2HYFcAXwAbFQ3HmAis8JLNmxRdTC9DwQL7sGJd4CAUYimPxuQHYkMNg1EELNP85YqFwqraLd4ovz6UeeekFLoCKiu\
    --btc-amount "0.0000135 BTC" --xmr-amount "0.001 XMR"\
    --network testnet --arb-blockchain bitcoin --acc-blockchain monero\
    --maker-role Bob --cancel-timelock 4 --punish-timelock 5 --fee-strategy "1500 satoshi/kvB"\
    --public-ip-addr {your-public-ip} --public-port 7067
```

:mag_right: Again, you must own the Bitcoin and Monero addresses given in arguments!

The `btc-addr` and `xmr-addr` are your external wallet addresses, where the coins will end up upon successful or failure cases. They are followed by the amounts exchanged. Assets and networks defaults to Bitcoin and Monero on testnet (Bitcoin testnet3, Monero stagenet).

The role for the maker is specified in the deal with `--maker-role`. `Alice` sells moneroj for bitcoins, `Bob` sells bitcoins for moneroj. Timelock parameters are set to **4** and **5** (testnet values, for mainnet this should be bigger) for cancel and punish and the transaction fee that must be applied is **1500 satoshi per kilo virtual bytes, i.e. 1.5 sat per virtual byte**.

Here the maker will send bitcoins and will receive moneroj in her `54EYTy2HYFcAXwAbFQ3HmAis8JLNmxRdTC9DwQL7sGJd4CAUYimPxuQHYkMNg1EELNP85YqFwqraLd4ovz6UeeekFLoCKiu` address if the swap is successful.

`--public-ip-addr` (default to `127.0.0.1`) and `--public-port` (default to `7067`) are used in the deal for the taker to connect.

:mag_right: To enable a taker to connect and take the deal the `public-ip-addr:public-port` must be accessible and answered by the `peerd` bound to the configured bind ip and port in your `farcasterd.toml` configuration file.

**The deal result**

The `make` command will output an encoded **deal** that can be shared with potential takers. As a maker, your `farcasterd` registers this deal, and waits for someone to connect through `peerd` and take it. A taker in her turn takes the deal and initiates a swap with the maker.

Follow your `farcasterd` logs (**you can fine tune your log with `RUST_LOG` environment variable, e.g. `RUST_LOG="farcaster_node=debug,microservices=debug"`**) and fund the swap with the bitcoins or moneroj when the log asks for this. At the end coins are swapped successfully, or - less ideally - refunded. Follow the progress through `swap-cli progress <swapid>`. To list the swap ids of the running swaps, use `swap-cli ls`.

## Manage deals

You can list registered deals in your node with the command:
```
swap-cli list-deals [--select <SELECT>]
```

The selector can have a value of: `open`, `inprogress`, `ended`, `all`

To remove a deal you can use the command:
```
swap-cli revoke-deal <DEAL>
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

