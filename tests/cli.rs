use std::time;
use utils::fc::*;

use farcaster_node::bus::info::MadeOffer;

#[macro_use]
extern crate log;

mod utils;

#[tokio::test]
#[ignore]
async fn cli_make_offer() {
    let (_, data_dir_maker, _, _) = setup_clients().await;

    // Allow some time for the microservices to start and register each other
    tokio::time::sleep(time::Duration::from_secs(10)).await;

    let mut args = vec![
        "make",
        "--btc-addr",
        "tb1q4gj53tuew3e6u4a32kdtle2q72su8te39dpceq",
        "--xmr-addr",
        "55LTR8KniP4LQGJSPtbYDacR7dz8RBFnsfAKMaMuwUNYX6aQbBcovzDPyrQF9KXF9tVU6Xk3K8no1BywnJX6GvZX8yJsXvt",
        "--btc-amount",
        "101 BTC",
        "--xmr-amount",
        "100 XMR",
        "--network",
        "Testnet",
        "--arb-blockchain",
        "Bitcoin",
        "--acc-blockchain",
        "Monero",
        "--maker-role",
        "Alice",
        "--cancel-timelock",
        "10",
        "--punish-timelock",
        "30",
        "--fee-strategy",
        "1 satoshi/vByte",
        "-p",
        "9376",
    ];
    args.append(&mut data_dir_maker.iter().map(std::ops::Deref::deref).collect());

    let res: MadeOffer = cli(args).unwrap();
    println!("{:?}", res);

    // clean up processes
    kill_all();
}
