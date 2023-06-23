You can choose to run the node directly on your machine or inside some containers. For a quick setup, containers might be your best bet assuming you already have Docker setup on your machine.

Depending on the chosen installation method:

- continue with [run on your machine](#run-on-your-machine)
- or [run with docker](#run-with-docker)

They provide instructions on how to launch the swap node, codenamed `farcasterd`, and interact with it with the `swap-cli` command line tool.

## Run on your machine

Follow the instruction in the [Install guide](./Install-guide) to compile sources on your machine, either from the git repo or using `cargo install`. When done you can now launch the services needed for the swap.

First you need to run a `monero rpc wallet` to manage in-transit moneroj. If you have it already installed on your machine you can run (we are assuming a monero stagenet <-> bitcoin testnet swap, else replace the given flags with `--rpc-bind-port 18083` and `--daemon-host node.community.rino.io:18081`)

```
monero-wallet-rpc --stagenet --rpc-bind-port 38083\
    --disable-rpc-login\
    --daemon-host stagenet.community.rino.io:38081\
    --trusted-daemon\
    --password "soMeDummYPasSwOrd"\
    --wallet-dir ~/.fc_monero_wallets
```

or you can use one of our Docker images as described [here](./Using-Docker#run-the-monero-rpc-wallet).

Now that you have a working Monero RPC wallet `farcasterd` can connect to, you can launch the node and follow the logs to see what's happening. Launch the node with `farcasterd` and the rest is automatic

```
farcasterd
```

:mag_right: You can find more details below about the [configuration](#configuration) if you need to customize its behavior.

Once `farcasterd` is up & running you can issue commands to control its actions with a client. Within this repo the included client is `swap-cli`, a command line tool that allows you to issue commands. An other alternative is using [Farcaster GUI (beta)](https://github.com/farcaster-project/farcaster-gui), for this you need to follow the instructions and enable the gRPC interface on your node.

If you launched `farcasterd` with the default paramters (the `--data-dir` argument or `-d` will point to the default folder `.farcaster` in you home folder), `swap-cli` will be able to connect to `farcasterd` without further configuration, otherwise make sure you pass the same values to `farcasterd` and `swap-cli`.

You can now get information about the node with the command

```
swap-cli info
```

Run `swap-cli help` for more details about all the available commands.

Commands you should know: `swap-cli info` gives a general overview of the node, `swap-cli ls` (or `swap-cli list-swaps`) lists the ongoing swaps and `swap-cli progress <swap_id>` give the state of a given swap.

With those commands and `farcasterd` logs you should be able to follow the state of your swaps.

Check out the documentation on configuration and usage for more advanced setups and to learn how to make and take deals.

## Run with docker

If you did use Docker you are already all set up, check out [Using Docker](./Using-Docker) for more details. Run `docker compose up -d` if you haven't yet, and the node and the wallet will start running. You can interact with the `farcasterd` container using the cli already setup inside the container via

```
docker compose exec farcasterd swap-cli info
```

Commands you should know: `swap-cli info` gives a general overview of the node, `swap-cli ls` lists the ongoing swaps and `swap-cli progress <swap_id>` gives the state of a given swap (use `--follow` to see live update progression).

With those commands and `farcasterd` logs (attach to the log with `docker compose logs -f --no-log-prefix farcasterd`), you should be able to follow your swaps.

Check out the documentation on configuration and usage for more advanced setups and to learn how to make and take deals.

## Configuration

`farcasterd` can be configured through a `.toml` file located by default at `~/.farcaster/farcasterd.toml` (for Linux and BSD, macOS will use `/Users/{user}/Library/Application Support/Farcaster/`). If no file is found, `farcasterd` is launched with some default values. You can see an example [here](https://github.com/farcaster-project/farcaster-node/blob/main/farcasterd.toml).

**Syncers**

This section configures the daemons' connection URLs for the three possible networks: _mainnet_, _testnet_, _local_:

```toml
[syncers.{network}]
electrum_server = ""
monero_daemon = ""
monero_rpc_wallet = ""
```

:mag_right: The default config for _local_ network is set to `null`, this network is used by the developers and you can ignore it.

### Use public infrastructure

To help quickly test and avoid running the entire infrastructure on your machine, you can make use of public nodes. Following is a non-exhaustive list of public nodes.

Only blockchain daemons and electrum servers are listed, you should always run your own `monero rpc wallet` locally.

**Mainnet**

| daemon            | value                                                |
| ----------------- | ---------------------------------------------------- |
| `electrum server` | `ssl://blockstream.info:700` **(default)**           |
| `monero daemon`   | `http://node.community.rino.io:18081` **(default)**  |

**Testnet/Stagenet**

| daemon            | value                                                   |
| ----------------- | ------------------------------------------------------- |
| `electrum server` | `ssl://blockstream.info:993` **(default)**              |
| `monero daemon`   | `http://stagenet.community.rino.io:38081` **(default)** |

