You can choose to run the node directly on your machine or inside some containers. For a quick setup, containers might be your best bet assuming you already have Docker setup.

Depending on the chosen installation method:

- you now have to continue to [run on your machine](#run-on-your-machine)
- or continue to [run with docker](#run-with-docker)

They provide instructions on how to launch the swap node, codenamed `farcasterd`, and interact with it with `swap-cli`.

## Run on your machine

Follow the instruction in the [Install guide](./Install-guide) to compile sources on your machine. When done you can now launch the services needed for the swap.

First you need to run a `monero rpc wallet` to manage in-transit moneroj. If you have it already installed on your machine you can run

```
monero-wallet-rpc --stagenet --rpc-bind-port 38083\
    --disable-rpc-login\
    --daemon-host stagenet.community.rino.io:38081\
    --trusted-daemon\
    --password "soMeDummYPasSwOrd"\
    --wallet-dir ~/.fc_monero_wallets
```

or you can use one of our Docker images as described [here](./Using-Docker#run-the-monero-rpc-wallet).

Now that you have a working Monero RPC wallet to connect you can launch the node, and follow the logs to see what's happening. Launch the node with

```
farcasterd
```

:mag_right: You can find more details below about the [configuration](#configuration) if you need to customize some values.

Once `farcasterd` is up & running you can issue commands to control its actions with a client. For the time being, only one client is provided within this repo: `swap-cli`.

If you launched `farcasterd` with the default paramters (the `--data-dir` argument or `-d` will point to the default folder `.farcaster` in you home folder), `swap-cli` will be able to connect to `farcasterd` without further configuration. You can get information about the node with

```
swap-cli info
```

Run `swap-cli help` command for more details about available commands.

Commands you should know: `swap-cli info` gives a general overview of the node, `swap-cli ls` lists the ongoing swaps and `swap-cli progress <swap_id>` give the state of a given swap.

With those commands and `farcasterd` logs you should be able to follow your swaps.

Check out the documentation on configuration and usage for more advanced setups and to learn how to make and take offers.

## Run with docker

If you did use Docker you are already all set up, check out [Using Docker](./Using-Docker) for more details. Run `docker-compose up -d` if you haven't yet, and the node and the wallet will start running. You can interact with the `farcasterd` container using the cli already setup inside the container via

```
docker-compose exec farcasterd swap-cli info
```

Commands you should know: `swap-cli info` gives a general overview of the node, `swap-cli ls` lists the ongoing swaps and `swap-cli progress <swap_id>` gives the state of a given swap (use `--follow` to see live update progression).

With those commands and `farcasterd` logs (attach to the log with `docker-compose logs -f --no-log-prefix farcasterd`), you should be able to follow your swaps.

Check out the documentation on configuration and usage for more advanced setups and to learn how to make and take offers.

## Configuration

`farcasterd` can be configured through a `.toml` file located by default at `~/.farcaster/farcasterd.toml` (for Linux and BSD, macOS will use `/Users/{user}/Library/Application Support/Farcaster/`). If no file is found, `farcasterd` is launched with some default values. You can see an example [here](https://github.com/farcaster-project/farcaster-node/blob/main/farcasterd.toml).

**Syncers**

This entry configures the daemons' connection URLs for the three possible networks: _mainnet_, _testnet_, _local_:

```toml
[syncers.{network}]
electrum_server = ""
monero_daemon = ""
monero_rpc_wallet = ""
```

:mag_right: The default config for _local_ network is set to `null`.

### :bulb: Use public infrastructure

To help quickly test and avoid running the entire infrastructure on your machine, you can make use of public nodes. Following is a non-exhaustive list of public nodes.

Only blockchain daemons and electrum servers are listed, you should always run your own `monero rpc wallet`.

**Mainnet**

| daemon            | value                                                |
| ----------------- | ---------------------------------------------------- |
| `electrum server` | `ssl://blockstream.info:700` **(default)**           |
| `monero daemon`   | `http://node.community.rino.io:18081`                |
| `monero daemon`   | `http://node.monerooutreach.org:18081` **(default)** |

**Testnet/Stagenet**

| daemon            | value                                                   |
| ----------------- | ------------------------------------------------------- |
| `electrum server` | `ssl://blockstream.info:993` **(default)**              |
| `monero daemon`   | `http://stagenet.community.rino.io:38081` **(default)** |
