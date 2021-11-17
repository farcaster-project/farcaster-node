# Running functional tests

Run the tests with:

```
docker-compose up -d
sudo chown -R $USER data_dir
cargo test --workspace --all-targets --all-features --no-fail-fast -- --ignored --test-threads=1
```

## Steps

By default the functional tests are not run when executing `cargo test`. To run them, run `cargo test --workspace --all-targets --all-features --no-fail-fast -- --ignored --test-threads=1`.

Before running the functional tests, start the docker containers first with `docker-compose up -d`.

Docker creates and mounts a volume in the `data_dir` directory. The owner of this directory is the root user, so ownership has to be changed before running the functional tests with `sudo chown -R $USER data_dir`.

Alternatively, the regtest setup can be run directly on the host, with the tests expecting the following endpoints:

- Bitcoind rpc cookie at `./tests/data_dir/regtest/.cookie`
- Bitcoind at `http://localhost:18443`
- Electrs at `tcp://localhost:50001`
- Monerod at `http://localhost:18081` run with arguments `--regtest --offline --fixed-difficulty 1`
- Two instances of Monero-wallet-rpc at `http://localhost:18083` and `htp://localhost:18084` run with arguments `--disable-rpc-login --wallet-dir wallets`
