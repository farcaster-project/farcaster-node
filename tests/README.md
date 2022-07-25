# Running functional tests

Start needed containers for supporting the tests with:

```
docker-compose pull
docker-compose up -d
```

Place your specific test name after `cargo test`, the following command runs all tests:

```
RUST_LOG="farcaster_node=debug,microservices=debug" cargo test --workspace --all-targets --all-features --no-fail-fast -- --ignored --test-threads=1
```

Stop the tests and remove the volume:

```
docker-compose down -v
```

## Steps

By default the functional tests are not run when executing `cargo test` because of the `#[ignore]` directive. To run them, see the above cargo test command with `-- --ignored`.

Before running the functional tests, start the docker containers first with `docker-compose up -d`.

Alternatively, the regtest setup can be run directly on the host, with the tests expecting the following endpoints:

- Bitcoind at `http://localhost:18443|18444`
- Electrs at `tcp://localhost:60401`
- Monerod at `http://localhost:18081|18082` run with arguments `--regtest --offline --fixed-difficulty 1`
- Three instances of Monero-wallet-rpc at `http://localhost:18083|18084|18085` run with arguments `--disable-rpc-login --wallet-dir wallets`
- Monero lws at `http://localhost:38884`
