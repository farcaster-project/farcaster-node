### Running function tests

To run the functional tests, start the docker containers first with
`docker-compose up`.

The regtest setup can be run directly on the host, with the tests expecting the
following endpoints:

- Bitcoind rpc cookie at `./tests/data_dir/regtest/.cookie`
- Bitcoind at `http://localhost:18443`
- Electrs at `tcp://localhost:50001`
- Monerod at `http://localhost:18081` with flags `--regtest --offline --fixed-difficulty 1`
- Two instances of Monero-wallet-rpc at `http://localhost:18083` and `htp://localhost:18084` with flags `--disable-rpc-login --wallet-dir wallets`

