# Running functional tests

Start needed containers for supporting the tests with:

```
docker-compose pull
docker-compose up -d
```

Run `bitcoin` tests:

```
cargo test bitcoin --workspace --all-targets --all-features --no-fail-fast -- --ignored --test-threads=1 --nocapture
```

Run `monero` tests:

```
cargo test monero --workspace --all-targets --all-features --no-fail-fast -- --ignored --test-threads=1 --nocapture
```

Run `swap` tests:

```
cargo test swap --workspace --all-targets --all-features --no-fail-fast -- --ignored --test-threads=1 --nocapture
```

Stop the tests and remove the volume

```
docker-compose down -v
```

## Steps

By default the functional tests are not run when executing `cargo test`. To run them, run `cargo test --workspace --all-targets --all-features --no-fail-fast -- --ignored --test-threads=1`.

Before running the functional tests, start the docker containers first with `docker-compose up -d`.

Docker creates and mounts a volume in the `data_dir` directory. The owner of this directory is the root user, so ownership has to be changed before running the functional tests with `sudo chown -R $USER data_dir`.

Alternatively, the regtest setup can be run directly on the host, with the tests expecting the following endpoints:

- Bitcoind rpc cookie at `./tests/data_dir/regtest/.cookie`
- Bitcoind at `http://localhost:18443`
- Electrs at `tcp://localhost:60401`
- Monerod at `http://localhost:18081` run with arguments `--regtest --offline --fixed-difficulty 1`
- Three instances of Monero-wallet-rpc at `http://localhost:18083|18084|18085` run with arguments `--disable-rpc-login --wallet-dir wallets`
