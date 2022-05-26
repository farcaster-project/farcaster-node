#[macro_use]
extern crate log;

use crate::farcaster::farcaster_client::FarcasterClient;
use bitcoincore_rpc::{Client, RpcApi};
use farcaster_core::swap::SwapId;
use farcaster_node::rpc::request::BitcoinFundingInfo;
use farcaster_node::rpc::request::MoneroFundingInfo;
use farcaster_node::rpc::request::NodeInfo;
use futures::future::join_all;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::io;
use std::process;
use std::sync::Arc;
use std::time;
use sysinfo::{ProcessExt, System, SystemExt};
use tokio::sync::Mutex;
use tonic::transport::Endpoint;

use std::collections::HashMap;
use std::env;
use std::str;
use std::str::FromStr;

use ntest::timeout;

use utils::config;

mod utils;

const ALLOWED_RETRIES: u32 = 180;

#[tokio::test]
#[timeout(600000)]
#[ignore]
async fn swap_bob_maker_normal() {
    let execution_mutex = Arc::new(Mutex::new(0));
    let bitcoin_rpc = Arc::new(bitcoin_setup());
    let (monero_regtest, monero_wallet) = monero_setup().await;

    let (farcasterd_maker, data_dir_maker, farcasterd_taker, data_dir_taker) =
        setup_farcaster_clients().await;

    let (xmr_dest_wallet_name, bitcoin_address, swap_id) = make_and_take_offer(
        data_dir_maker.clone(),
        data_dir_taker.clone(),
        "Bob".to_string(),
        Arc::clone(&bitcoin_rpc),
        Arc::clone(&monero_wallet),
        bitcoin::Amount::from_str("1 BTC").unwrap(),
        monero::Amount::from_str_with_denomination("1 XMR").unwrap(),
    )
    .await;

    run_swap(
        swap_id,
        data_dir_taker,
        data_dir_maker,
        Arc::clone(&bitcoin_rpc),
        bitcoin_address,
        monero_regtest,
        Arc::clone(&monero_wallet),
        xmr_dest_wallet_name,
        execution_mutex,
    )
    .await;

    cleanup_processes(vec![farcasterd_maker, farcasterd_taker]);
}

pub mod farcaster {
    tonic::include_proto!("farcaster");
}

use farcaster::InfoRequest;

#[tokio::test]
#[ignore]
async fn grpc_server_functional_test() {
    let (farcasterd_maker, _, farcasterd_taker, _) = setup_farcaster_clients().await;

    // Allow some time for the microservices to start and register each other
    tokio::time::sleep(time::Duration::from_secs(10)).await;

    let channel = Endpoint::from_static("http://[::1]:50051")
        .connect()
        .await
        .unwrap();

    let mut farcaster_client = FarcasterClient::new(channel.clone());
    let request = tonic::Request::new(InfoRequest { id: 0 });
    let response = farcaster_client.info(request).await;
    assert_eq!(response.unwrap().into_inner().id, 0);
    cleanup_processes(vec![farcasterd_maker, farcasterd_taker]);
}

#[tokio::test]
#[timeout(600000)]
#[ignore]
async fn swap_bob_maker_refund_race_cancel() {
    let execution_mutex = Arc::new(Mutex::new(0));
    let bitcoin_rpc = Arc::new(bitcoin_setup());
    let (monero_regtest, monero_wallet) = monero_setup().await;

    let (farcasterd_maker, data_dir_maker, farcasterd_taker, data_dir_taker) =
        setup_farcaster_clients().await;

    let (xmr_dest_wallet_name, bitcoin_address, swap_id) = make_and_take_offer(
        data_dir_maker.clone(),
        data_dir_taker.clone(),
        "Bob".to_string(),
        Arc::clone(&bitcoin_rpc),
        Arc::clone(&monero_wallet),
        bitcoin::Amount::from_str("1 BTC").unwrap(),
        monero::Amount::from_str_with_denomination("1 XMR").unwrap(),
    )
    .await;

    run_refund_swap_race_cancel(
        swap_id,
        data_dir_taker,
        data_dir_maker,
        Arc::clone(&bitcoin_rpc),
        bitcoin_address,
        monero_regtest,
        Arc::clone(&monero_wallet),
        xmr_dest_wallet_name,
        execution_mutex,
    )
    .await;

    cleanup_processes(vec![farcasterd_maker, farcasterd_taker]);
}

#[tokio::test]
#[timeout(600000)]
#[ignore]
async fn swap_bob_maker_refund_kill_alice_after_funding() {
    let execution_mutex = Arc::new(Mutex::new(0));
    let bitcoin_rpc = Arc::new(bitcoin_setup());
    let (_monero_regtest, monero_wallet) = monero_setup().await;

    let (farcasterd_maker, data_dir_maker, farcasterd_taker, data_dir_taker) =
        setup_farcaster_clients().await;

    let (_xmr_dest_wallet_name, bitcoin_address, swap_id) = make_and_take_offer(
        data_dir_maker.clone(),
        data_dir_taker.clone(),
        "Bob".to_string(),
        Arc::clone(&bitcoin_rpc),
        Arc::clone(&monero_wallet),
        bitcoin::Amount::from_str("1 BTC").unwrap(),
        monero::Amount::from_str_with_denomination("1 XMR").unwrap(),
    )
    .await;

    run_refund_swap_kill_alice_after_funding(
        swap_id,
        data_dir_taker,
        data_dir_maker,
        Arc::clone(&bitcoin_rpc),
        bitcoin_address,
        Arc::clone(&monero_wallet),
        execution_mutex,
        farcasterd_taker,
    )
    .await;

    cleanup_processes(vec![farcasterd_maker]);
}

#[tokio::test]
#[timeout(600000)]
#[ignore]
async fn swap_bob_maker_refund_alice_does_not_fund() {
    let execution_mutex = Arc::new(Mutex::new(0));
    let bitcoin_rpc = Arc::new(bitcoin_setup());
    let (_monero_regtest, monero_wallet) = monero_setup().await;

    let (farcasterd_maker, data_dir_maker, farcasterd_taker, data_dir_taker) =
        setup_farcaster_clients().await;

    let (_xmr_dest_wallet_name, bitcoin_address, swap_id) = make_and_take_offer(
        data_dir_maker.clone(),
        data_dir_taker.clone(),
        "Bob".to_string(),
        Arc::clone(&bitcoin_rpc),
        Arc::clone(&monero_wallet),
        bitcoin::Amount::from_str("1 BTC").unwrap(),
        monero::Amount::from_str_with_denomination("1 XMR").unwrap(),
    )
    .await;

    run_refund_swap_alice_does_not_fund(
        swap_id,
        data_dir_taker,
        data_dir_maker,
        Arc::clone(&bitcoin_rpc),
        bitcoin_address,
        execution_mutex,
    )
    .await;

    cleanup_processes(vec![farcasterd_maker, farcasterd_taker]);
}

#[tokio::test]
#[timeout(600000)]
#[ignore]
async fn swap_bob_maker_punish_kill_bob() {
    let execution_mutex = Arc::new(Mutex::new(0));
    let bitcoin_rpc = Arc::new(bitcoin_setup());
    let (monero_regtest, monero_wallet) = monero_setup().await;

    let (farcasterd_maker, data_dir_maker, farcasterd_taker, data_dir_taker) =
        setup_farcaster_clients().await;

    let (_xmr_dest_wallet_name, bitcoin_address, swap_id) = make_and_take_offer(
        data_dir_maker.clone(),
        data_dir_taker.clone(),
        "Bob".to_string(),
        Arc::clone(&bitcoin_rpc),
        Arc::clone(&monero_wallet),
        bitcoin::Amount::from_str("1 BTC").unwrap(),
        monero::Amount::from_str_with_denomination("1 XMR").unwrap(),
    )
    .await;

    run_punish_swap_kill_bob_before_monero_funding(
        swap_id,
        data_dir_taker,
        data_dir_maker,
        Arc::clone(&bitcoin_rpc),
        bitcoin_address,
        monero_regtest,
        Arc::clone(&monero_wallet),
        execution_mutex,
        farcasterd_maker,
    )
    .await;

    cleanup_processes(vec![farcasterd_taker]);
}

#[tokio::test]
#[timeout(600000)]
#[ignore]
async fn swap_alice_maker() {
    let execution_mutex = Arc::new(Mutex::new(0));
    let bitcoin_rpc = Arc::new(bitcoin_setup());
    let (monero_regtest, monero_wallet) = monero_setup().await;

    let (farcasterd_maker, data_dir_maker, farcasterd_taker, data_dir_taker) =
        setup_farcaster_clients().await;

    let (xmr_dest_wallet_name, bitcoin_address, swap_id) = make_and_take_offer(
        data_dir_maker.clone(),
        data_dir_taker.clone(),
        "Alice".to_string(),
        Arc::clone(&bitcoin_rpc),
        Arc::clone(&monero_wallet),
        bitcoin::Amount::from_str("1 BTC").unwrap(),
        monero::Amount::from_str_with_denomination("1 XMR").unwrap(),
    )
    .await;

    run_swap(
        swap_id,
        data_dir_maker,
        data_dir_taker,
        Arc::clone(&bitcoin_rpc),
        bitcoin_address,
        monero_regtest,
        Arc::clone(&monero_wallet),
        xmr_dest_wallet_name,
        execution_mutex,
    )
    .await;

    cleanup_processes(vec![farcasterd_maker, farcasterd_taker]);
}

#[derive(Debug, Clone)]
struct SwapParams {
    data_dir_bob: Vec<String>,
    data_dir_alice: Vec<String>,
    xmr_dest_wallet_name: String,
    destination_btc_address: bitcoin::Address,
}

#[tokio::test]
#[timeout(800000)]
#[ignore]
async fn swap_parallel_execution() {
    let execution_mutex = Arc::new(Mutex::new(0));
    let bitcoin_rpc = Arc::new(bitcoin_setup());
    let (monero_regtest, monero_wallet) = monero_setup().await;

    let (farcasterd_maker, data_dir_maker, farcasterd_taker, data_dir_taker) =
        setup_farcaster_clients().await;

    let previous_offers: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let previous_swap_ids: Arc<Mutex<HashSet<SwapId>>> = Arc::new(Mutex::new(HashSet::new()));

    let mut res = Vec::new();
    for i in 0..5 {
        let xmr_amount = format!("1.{} XMR", i);
        res.push(make_and_take_offer_parallel(
            data_dir_maker.clone(),
            data_dir_taker.clone(),
            "Bob".to_string(),
            Arc::clone(&bitcoin_rpc),
            Arc::clone(&monero_wallet),
            bitcoin::Amount::from_str("1 BTC").unwrap(),
            monero::Amount::from_str_with_denomination(&xmr_amount).unwrap(),
            Arc::clone(&previous_offers),
            Arc::clone(&previous_swap_ids),
        ));
    }

    let mut results = join_all(res).await;
    let mut swap_info: HashMap<SwapId, SwapParams> = results
        .drain(..)
        .map(|(xmr_dest_wallet_name, destination_btc_address, swap_id)| {
            (
                swap_id,
                SwapParams {
                    data_dir_bob: data_dir_maker.clone(),
                    data_dir_alice: data_dir_taker.clone(),
                    xmr_dest_wallet_name,
                    destination_btc_address,
                },
            )
        })
        .collect();

    let mut res = Vec::new();
    for i in 0..5 {
        let xmr_amount = format!("1.{} XMR", i);
        res.push(make_and_take_offer_parallel(
            data_dir_maker.clone(),
            data_dir_taker.clone(),
            "Alice".to_string(),
            Arc::clone(&bitcoin_rpc),
            Arc::clone(&monero_wallet),
            bitcoin::Amount::from_str("1 BTC").unwrap(),
            monero::Amount::from_str_with_denomination(&xmr_amount).unwrap(),
            Arc::clone(&previous_offers),
            Arc::clone(&previous_swap_ids),
        ));
    }

    let mut results = join_all(res).await;
    let swap_info_alice: HashMap<SwapId, SwapParams> = results
        .drain(..)
        .map(|(xmr_dest_wallet_name, destination_btc_address, swap_id)| {
            (
                swap_id,
                SwapParams {
                    data_dir_bob: data_dir_taker.clone(),
                    data_dir_alice: data_dir_maker.clone(),
                    xmr_dest_wallet_name,
                    destination_btc_address,
                },
            )
        })
        .collect();

    swap_info.extend(swap_info_alice);

    run_swaps_parallel(
        swap_info,
        Arc::clone(&bitcoin_rpc),
        monero_regtest.clone(),
        Arc::clone(&monero_wallet.clone()),
        Arc::clone(&execution_mutex),
    )
    .await;
    cleanup_processes(vec![farcasterd_maker, farcasterd_taker]);
}

async fn setup_farcaster_clients() -> (process::Child, Vec<String>, process::Child, Vec<String>) {
    // data directories
    let data_dir_maker = vec!["-d".to_string(), "tests/fc1".to_string()];
    let data_dir_taker = vec!["-d".to_string(), "tests/fc2".to_string()];

    // If we are in CI we use .ci.toml files, otherwise .toml
    let ctx = env::var("CI").unwrap_or("false".into());
    let ext = if ctx == "false" { ".toml" } else { ".ci.toml" };

    let farcasterd_maker_args = farcasterd_args(
        data_dir_maker.clone(),
        vec!["-vvv", "--config", &format!("tests/cfg/fc1{}", ext)],
        vec!["2>&1", "|", "tee", "-a", "tests/fc1.log"],
    );
    let farcasterd_taker_args = farcasterd_args(
        data_dir_taker.clone(),
        vec!["-vvv", "--config", &format!("tests/cfg/fc2{}", ext)],
        vec!["2>&1", "|", "tee", "-a", "tests/fc2.log"],
    );

    let farcasterd_maker = launch("../farcasterd", farcasterd_maker_args).unwrap();
    let farcasterd_taker = launch("../farcasterd", farcasterd_taker_args).unwrap();
    (
        farcasterd_maker,
        data_dir_maker,
        farcasterd_taker,
        data_dir_taker,
    )
}

#[allow(clippy::too_many_arguments)]
async fn run_refund_swap_race_cancel(
    swap_id: SwapId,
    data_dir_alice: Vec<String>,
    data_dir_bob: Vec<String>,
    bitcoin_rpc: Arc<bitcoincore_rpc::Client>,
    funding_btc_address: bitcoin::Address,
    monero_regtest: monero_rpc::RegtestDaemonClient,
    monero_wallet: Arc<Mutex<monero_rpc::WalletClient>>,
    monero_dest_wallet_name: String,
    execution_mutex: Arc<Mutex<u8>>,
) {
    let cli_bob_progress_args: Vec<String> = progress_args(data_dir_bob.clone(), swap_id.clone());
    let cli_alice_progress_args: Vec<String> =
        progress_args(data_dir_alice.clone(), swap_id.clone());
    let cli_bob_needs_funding_args: Vec<String> =
        needs_funding_args(data_dir_bob, "bitcoin".to_string());
    let cli_alice_needs_funding_args: Vec<String> =
        needs_funding_args(data_dir_alice, "monero".to_string());

    bitcoin_rpc
        .generate_to_address(1, &reusable_btc_address())
        .unwrap();

    // run until bob has the btc funding address
    let (address, amount) =
        retry_until_bitcoin_funding_address(swap_id.clone(), cli_bob_needs_funding_args.clone())
            .await;

    // fund the bitcoin address
    let lock = execution_mutex.lock().await;
    bitcoin_rpc
        .send_to_address(&address, amount, None, None, None, None, None, None)
        .unwrap();

    println!("waiting for AliceState(RefundSigs");
    retry_until_finish_state_transition(
        cli_alice_progress_args.clone(),
        "AliceState(RefundSigs".to_string(),
    )
    .await;

    // run until BobState(CoreArb) is received
    println!("waiting for BobState(CoreArb)");
    retry_until_finish_state_transition(
        cli_bob_progress_args.clone(),
        "BobState(CoreArb)".to_string(),
    )
    .await;

    // run until the funding infos are cleared again
    println!("waiting for the bitcoin funding info to clear");
    retry_until_funding_info_cleared(swap_id.clone(), cli_bob_needs_funding_args.clone()).await;

    tokio::time::sleep(time::Duration::from_secs(10)).await;

    // generate some bitcoin blocks to finalize the bitcoin arb lock tx
    bitcoin_rpc
        .generate_to_address(3, &reusable_btc_address())
        .unwrap();

    // run until the alice has the monero funding address and fund it
    let (monero_address, monero_amount) =
        retry_until_monero_funding_address(swap_id, cli_alice_needs_funding_args.clone()).await;
    send_monero(Arc::clone(&monero_wallet), monero_address, monero_amount).await;

    tokio::time::sleep(time::Duration::from_secs(20)).await;

    // generate some bitcoin blocks for confirmations
    bitcoin_rpc
        .generate_to_address(20, &reusable_btc_address())
        .unwrap();

    tokio::time::sleep(time::Duration::from_secs(20)).await;

    // generate some bitcoin blocks to finalize the bitcoin cancel tx
    bitcoin_rpc
        .generate_to_address(3, &reusable_btc_address())
        .unwrap();

    // generate some bitcoin blocks for confirmations
    bitcoin_rpc
        .generate_to_address(20, &reusable_btc_address())
        .unwrap();
    tokio::time::sleep(time::Duration::from_secs(20)).await;

    // generate some bitcoin blocks to finalize the bitcoin refund tx
    bitcoin_rpc
        .generate_to_address(3, &reusable_btc_address())
        .unwrap();

    // run until the BobState(Finish(Failure(Refunded))) is received
    retry_until_finish_state_transition(
        cli_bob_progress_args.clone(),
        "BobState(Finish(Failure(Refunded)))".to_string(),
    )
    .await;

    // generate some blocks on bitcoin's side
    bitcoin_rpc
        .generate_to_address(1, &reusable_btc_address())
        .unwrap();

    let (_stdout, _stderr) = run("../swap-cli", cli_bob_progress_args.clone()).unwrap();

    // check that btc was received in the destination address
    let balance = bitcoin_rpc
        .get_received_by_address(&funding_btc_address, None)
        .unwrap();
    assert!(balance.as_sat() > 90000000);

    // cache the monero balance before sweeping
    let monero_wallet_lock = monero_wallet.lock().await;
    monero_wallet_lock
        .open_wallet(monero_dest_wallet_name.clone(), None)
        .await
        .unwrap();
    let before_balance = monero_wallet_lock.get_balance(0, None).await.unwrap();
    drop(monero_wallet_lock);

    // Sleep here to work around a race condition between pending
    // SweepXmrAddress requests and tx Acc Lock confirmations. If Acc Lock
    // confirmations are produced before the pending request is queued, no
    // action will take place after this point.
    tokio::time::sleep(time::Duration::from_secs(10)).await;

    // generate some blocks on monero's side
    monero_regtest
        .generate_blocks(10, reusable_xmr_address())
        .await
        .unwrap();

    // run until the BobState(Finish) is received
    retry_until_finish_state_transition(
        cli_alice_progress_args.clone(),
        "AliceState(Finish(Failure(Refunded)))".to_string(),
    )
    .await;

    monero_regtest
        .generate_blocks(1, reusable_xmr_address())
        .await
        .unwrap();

    let monero_wallet_lock = monero_wallet.lock().await;
    monero_wallet_lock
        .open_wallet(monero_dest_wallet_name, None)
        .await
        .unwrap();
    monero_wallet_lock.refresh(Some(1)).await.unwrap();
    let after_balance = monero_wallet_lock.get_balance(0, None).await.unwrap();
    drop(monero_wallet_lock);
    let delta_balance = after_balance.balance - before_balance.balance;
    assert!(delta_balance > 999660000000);
    drop(lock);
}

#[allow(clippy::too_many_arguments)]
async fn run_refund_swap_kill_alice_after_funding(
    swap_id: SwapId,
    data_dir_alice: Vec<String>,
    data_dir_bob: Vec<String>,
    bitcoin_rpc: Arc<bitcoincore_rpc::Client>,
    funding_btc_address: bitcoin::Address,
    monero_wallet: Arc<Mutex<monero_rpc::WalletClient>>,
    execution_mutex: Arc<Mutex<u8>>,
    alice_farcasterd: std::process::Child,
) {
    let cli_bob_progress_args: Vec<String> = progress_args(data_dir_bob.clone(), swap_id.clone());
    let cli_alice_progress_args: Vec<String> =
        progress_args(data_dir_alice.clone(), swap_id.clone());
    let cli_bob_needs_funding_args: Vec<String> =
        needs_funding_args(data_dir_bob, "bitcoin".to_string());
    let cli_alice_needs_funding_args: Vec<String> =
        needs_funding_args(data_dir_alice, "monero".to_string());

    bitcoin_rpc
        .generate_to_address(1, &reusable_btc_address())
        .unwrap();

    // run until bob has the btc funding address
    let (address, amount) =
        retry_until_bitcoin_funding_address(swap_id.clone(), cli_bob_needs_funding_args.clone())
            .await;

    // fund the bitcoin address
    let lock = execution_mutex.lock().await;
    bitcoin_rpc
        .send_to_address(&address, amount, None, None, None, None, None, None)
        .unwrap();

    println!("waiting for AliceState(RefundSigs");
    retry_until_finish_state_transition(
        cli_alice_progress_args.clone(),
        "AliceState(RefundSigs".to_string(),
    )
    .await;

    // run until BobState(CoreArb) is received
    println!("waiting for BobState(CoreArb)");
    retry_until_finish_state_transition(
        cli_bob_progress_args.clone(),
        "BobState(CoreArb)".to_string(),
    )
    .await;

    // run until the funding infos are cleared again
    println!("waiting for the bitcoin funding info to clear");
    retry_until_funding_info_cleared(swap_id.clone(), cli_bob_needs_funding_args.clone()).await;

    tokio::time::sleep(time::Duration::from_secs(10)).await;

    // generate some bitcoin blocks to finalize the bitcoin arb lock tx
    bitcoin_rpc
        .generate_to_address(3, &reusable_btc_address())
        .unwrap();

    // run until the alice has the monero funding address and fund it
    let (monero_address, monero_amount) =
        retry_until_monero_funding_address(swap_id, cli_alice_needs_funding_args.clone()).await;
    send_monero(Arc::clone(&monero_wallet), monero_address, monero_amount).await;

    // kill alice
    cleanup_processes(vec![alice_farcasterd]);

    tokio::time::sleep(time::Duration::from_secs(20)).await;

    // generate some bitcoin blocks for confirmations
    bitcoin_rpc
        .generate_to_address(20, &reusable_btc_address())
        .unwrap();

    tokio::time::sleep(time::Duration::from_secs(20)).await;

    // generate some bitcoin blocks to finalize the bitcoin cancel tx
    bitcoin_rpc
        .generate_to_address(3, &reusable_btc_address())
        .unwrap();

    // generate some bitcoin blocks for confirmations
    bitcoin_rpc
        .generate_to_address(20, &reusable_btc_address())
        .unwrap();
    tokio::time::sleep(time::Duration::from_secs(20)).await;

    // generate some bitcoin blocks to finalize the bitcoin refund tx
    bitcoin_rpc
        .generate_to_address(3, &reusable_btc_address())
        .unwrap();

    // run until the BobState(Finish(Failure(Refunded))) is received
    retry_until_finish_state_transition(
        cli_bob_progress_args.clone(),
        "BobState(Finish(Failure(Refunded)))".to_string(),
    )
    .await;

    // generate some blocks on bitcoin's side
    bitcoin_rpc
        .generate_to_address(1, &reusable_btc_address())
        .unwrap();

    let (_stdout, _stderr) = run("../swap-cli", cli_bob_progress_args.clone()).unwrap();

    // check that btc was received in the destination address
    let balance = bitcoin_rpc
        .get_received_by_address(&funding_btc_address, None)
        .unwrap();
    assert!(balance.as_sat() > 90000000);
    drop(lock);
}

#[allow(clippy::too_many_arguments)]
async fn run_refund_swap_alice_does_not_fund(
    swap_id: SwapId,
    data_dir_alice: Vec<String>,
    data_dir_bob: Vec<String>,
    bitcoin_rpc: Arc<bitcoincore_rpc::Client>,
    funding_btc_address: bitcoin::Address,
    execution_mutex: Arc<Mutex<u8>>,
) {
    let cli_alice_progress_args: Vec<String> =
        progress_args(data_dir_alice.clone(), swap_id.clone());
    let cli_bob_progress_args: Vec<String> = progress_args(data_dir_bob.clone(), swap_id.clone());
    let cli_bob_needs_funding_args: Vec<String> =
        needs_funding_args(data_dir_bob, "bitcoin".to_string());
    let cli_alice_needs_funding_args: Vec<String> =
        needs_funding_args(data_dir_alice, "monero".to_string());

    bitcoin_rpc
        .generate_to_address(1, &reusable_btc_address())
        .unwrap();

    // run until bob has the btc funding address
    let (address, amount) =
        retry_until_bitcoin_funding_address(swap_id.clone(), cli_bob_needs_funding_args.clone())
            .await;

    // fund the bitcoin address
    let lock = execution_mutex.lock().await;
    bitcoin_rpc
        .send_to_address(&address, amount, None, None, None, None, None, None)
        .unwrap();

    println!("waiting for AliceState(RefundSigs");
    retry_until_finish_state_transition(
        cli_alice_progress_args.clone(),
        "AliceState(RefundSigs".to_string(),
    )
    .await;

    // run until BobState(CoreArb) is received
    println!("waiting for BobState(CoreArb)");
    retry_until_finish_state_transition(
        cli_bob_progress_args.clone(),
        "BobState(CoreArb)".to_string(),
    )
    .await;

    // run until the funding infos are cleared again
    println!("waiting for the bitcoin funding info to clear");
    retry_until_funding_info_cleared(swap_id.clone(), cli_bob_needs_funding_args.clone()).await;

    tokio::time::sleep(time::Duration::from_secs(10)).await;

    // generate some bitcoin blocks to finalize the bitcoin arb lock tx
    bitcoin_rpc
        .generate_to_address(3, &reusable_btc_address())
        .unwrap();

    // run until the alice has the monero funding address, but do not fund it
    retry_until_monero_funding_address(swap_id, cli_alice_needs_funding_args.clone()).await;

    // generate some bitcoin blocks for confirmations
    bitcoin_rpc
        .generate_to_address(20, &reusable_btc_address())
        .unwrap();

    tokio::time::sleep(time::Duration::from_secs(20)).await;

    bitcoin_rpc
        .generate_to_address(3, &reusable_btc_address())
        .unwrap();

    // run until the AliceState(Finish(Failure(Refunded))) is received
    retry_until_finish_state_transition(
        cli_alice_progress_args.clone(),
        "AliceState(Finish(Failure(Refunded)))".to_string(),
    )
    .await;

    // generate some bitcoin blocks for confirmations
    bitcoin_rpc
        .generate_to_address(20, &reusable_btc_address())
        .unwrap();
    tokio::time::sleep(time::Duration::from_secs(20)).await;

    bitcoin_rpc
        .generate_to_address(3, &reusable_btc_address())
        .unwrap();

    // run until the BobState(Finish(Failure(Refunded))) is received
    retry_until_finish_state_transition(
        cli_bob_progress_args.clone(),
        "BobState(Finish(Failure(Refunded)))".to_string(),
    )
    .await;

    // generate some blocks on bitcoin's side
    bitcoin_rpc
        .generate_to_address(1, &reusable_btc_address())
        .unwrap();

    let (_stdout, _stderr) = run("../swap-cli", cli_bob_progress_args.clone()).unwrap();

    // check that btc was received in the destination address
    let balance = bitcoin_rpc
        .get_received_by_address(&funding_btc_address, None)
        .unwrap();
    assert!(balance.as_sat() > 90000000);
    drop(lock);
}

#[allow(clippy::too_many_arguments)]
async fn run_punish_swap_kill_bob_before_monero_funding(
    swap_id: SwapId,
    data_dir_alice: Vec<String>,
    data_dir_bob: Vec<String>,
    bitcoin_rpc: Arc<bitcoincore_rpc::Client>,
    funding_btc_address: bitcoin::Address,
    monero_regtest: monero_rpc::RegtestDaemonClient,
    monero_wallet: Arc<Mutex<monero_rpc::WalletClient>>,
    execution_mutex: Arc<Mutex<u8>>,
    bob_farcasterd: std::process::Child,
) {
    let cli_bob_progress_args: Vec<String> = progress_args(data_dir_bob.clone(), swap_id.clone());
    let cli_alice_progress_args: Vec<String> =
        progress_args(data_dir_alice.clone(), swap_id.clone());
    let cli_bob_needs_funding_args: Vec<String> =
        needs_funding_args(data_dir_bob, "bitcoin".to_string());
    let cli_alice_needs_funding_args: Vec<String> =
        needs_funding_args(data_dir_alice, "monero".to_string());

    bitcoin_rpc
        .generate_to_address(1, &reusable_btc_address())
        .unwrap();

    // run until bob has the btc funding address
    let (address, amount) =
        retry_until_bitcoin_funding_address(swap_id.clone(), cli_bob_needs_funding_args.clone())
            .await;

    // fund the bitcoin address
    let lock = execution_mutex.lock().await;
    bitcoin_rpc
        .send_to_address(&address, amount, None, None, None, None, None, None)
        .unwrap();

    println!("waiting for AliceState(RefundSigs");
    retry_until_finish_state_transition(
        cli_alice_progress_args.clone(),
        "AliceState(RefundSigs".to_string(),
    )
    .await;

    // run until BobState(CoreArb) is received
    println!("waiting for BobState(CoreArb)");
    retry_until_finish_state_transition(
        cli_bob_progress_args.clone(),
        "BobState(CoreArb)".to_string(),
    )
    .await;

    // run until the funding infos are cleared again
    println!("waiting for the bitcoin funding info to clear");
    retry_until_funding_info_cleared(swap_id.clone(), cli_bob_needs_funding_args.clone()).await;

    tokio::time::sleep(time::Duration::from_secs(10)).await;

    // generate some bitcoin blocks to finalize the bitcoin arb lock tx
    bitcoin_rpc
        .generate_to_address(3, &reusable_btc_address())
        .unwrap();
    monero_regtest
        .generate_blocks(11, reusable_xmr_address())
        .await
        .unwrap();

    tokio::time::sleep(time::Duration::from_secs(20)).await;

    // kill bob
    cleanup_processes(vec![bob_farcasterd]);

    // run until alice has the monero funding address
    let (monero_address, monero_amount) =
        retry_until_monero_funding_address(swap_id, cli_alice_needs_funding_args.clone()).await;
    send_monero(Arc::clone(&monero_wallet), monero_address, monero_amount).await;

    tokio::time::sleep(time::Duration::from_secs(20)).await;

    // generate some bitcoin blocks for confirmations
    bitcoin_rpc
        .generate_to_address(20, &reusable_btc_address())
        .unwrap();

    println!("generated 20 bitcoin blocks");

    tokio::time::sleep(time::Duration::from_secs(20)).await;

    // generate some confirmations for the cancel tx
    bitcoin_rpc
        .generate_to_address(3, &reusable_btc_address())
        .unwrap();

    println!("generated 20 bitcoin blocks");

    tokio::time::sleep(time::Duration::from_secs(20)).await;

    // generate some bitcoin blocks for confirmations
    bitcoin_rpc
        .generate_to_address(20, &reusable_btc_address())
        .unwrap();
    println!("generated 20 bitcoin blocks");

    monero_regtest
        .generate_blocks(20, reusable_xmr_address())
        .await
        .unwrap();

    tokio::time::sleep(time::Duration::from_secs(20)).await;

    // generate some bitcoin blocks for confirmations
    bitcoin_rpc
        .generate_to_address(20, &reusable_btc_address())
        .unwrap();
    println!("generated 20 bitcoin blocks");

    tokio::time::sleep(time::Duration::from_secs(20)).await;

    // generate some confirmations for the cancel tx
    bitcoin_rpc
        .generate_to_address(3, &reusable_btc_address())
        .unwrap();

    println!("generated 20 bitcoin blocks");

    // run until the AliceState(Finish) is received
    retry_until_finish_state_transition(
        cli_alice_progress_args.clone(),
        "AliceState(Finish(Failure(Punished)))".to_string(),
    )
    .await;

    bitcoin_rpc
        .generate_to_address(1, &reusable_btc_address())
        .unwrap();
    println!("generated 20 bitcoin blocks");

    // check that btc was received in the destination address
    let balance = bitcoin_rpc
        .get_received_by_address(&funding_btc_address, None)
        .unwrap();
    assert!(balance.as_sat() > 90000000);
    drop(lock);
}

async fn make_and_take_offer_parallel(
    data_dir_maker: Vec<String>,
    data_dir_taker: Vec<String>,
    role: String,
    bitcoin_rpc: Arc<bitcoincore_rpc::Client>,
    monero_wallet: Arc<Mutex<monero_rpc::WalletClient>>,
    btc_amount: bitcoin::Amount,
    xmr_amount: monero::Amount,
    previous_offers: Arc<Mutex<HashSet<String>>>,
    previous_swap_ids: Arc<Mutex<HashSet<SwapId>>>,
) -> (String, bitcoin::Address, SwapId) {
    let maker_info_args = info_args(data_dir_maker.clone());
    let taker_info_args = info_args(data_dir_maker.clone());

    // test connection to farcasterd and check that swap-cli is in the correct place
    run("../swap-cli", maker_info_args.clone()).unwrap();

    let (xmr_address, xmr_address_wallet_name) =
        monero_new_dest_address(Arc::clone(&monero_wallet)).await;
    let btc_address = bitcoin_rpc.get_new_address(None, None).unwrap();
    let btc_addr = btc_address.to_string();
    let xmr_addr = xmr_address.to_string();

    let (_stdout, _stderr) = run("../swap-cli", taker_info_args.clone()).unwrap();

    let cli_make_args = make_offer_args(
        data_dir_maker.clone(),
        role,
        btc_addr.clone(),
        btc_amount,
        xmr_addr.clone(),
        xmr_amount,
    );
    let (_stdout, _stderr) = run("../swap-cli", cli_make_args).unwrap();

    // get offer strings
    let offer =
        retry_until_offer_parallel(maker_info_args.clone(), Arc::clone(&previous_offers)).await;

    let cli_take_args = take_offer_args(data_dir_taker.clone(), btc_addr, xmr_addr, offer.clone());
    run("../swap-cli", cli_take_args).unwrap();

    let swap_id =
        retry_until_swap_id_parallel(taker_info_args.clone(), Arc::clone(&previous_swap_ids)).await;

    (xmr_address_wallet_name, btc_address, swap_id)
}

async fn make_and_take_offer(
    data_dir_maker: Vec<String>,
    data_dir_taker: Vec<String>,
    role: String,
    bitcoin_rpc: Arc<bitcoincore_rpc::Client>,
    monero_wallet: Arc<Mutex<monero_rpc::WalletClient>>,
    btc_amount: bitcoin::Amount,
    xmr_amount: monero::Amount,
) -> (String, bitcoin::Address, SwapId) {
    let maker_info_args = info_args(data_dir_maker.clone());
    let taker_info_args = info_args(data_dir_maker.clone());

    // test connection to farcasterd and check that swap-cli is in the correct place
    run("../swap-cli", maker_info_args.clone()).unwrap();

    let (xmr_address, xmr_address_wallet_name) =
        monero_new_dest_address(Arc::clone(&monero_wallet)).await;
    let btc_address = bitcoin_rpc.get_new_address(None, None).unwrap();
    let btc_addr = btc_address.to_string();
    let xmr_addr = xmr_address.to_string();

    let (stdout, _stderr) = run("../swap-cli", taker_info_args.clone()).unwrap();
    let previous_swap_ids: HashSet<SwapId> =
        cli_output_to_node_info(stdout).swaps.drain(..).collect();

    let cli_make_args = make_offer_args(
        data_dir_maker.clone(),
        role,
        btc_addr.clone(),
        btc_amount,
        xmr_addr.clone(),
        xmr_amount,
    );
    let (_stdout, _stderr) = run("../swap-cli", cli_make_args).unwrap();

    // get offer strings
    let offers = retry_until_offer(maker_info_args.clone()).await;

    let cli_take_args = take_offer_args(
        data_dir_taker.clone(),
        btc_addr,
        xmr_addr,
        offers[0].clone(),
    );
    run("../swap-cli", cli_take_args).unwrap();

    let swap_id = retry_until_swap_id(taker_info_args.clone(), previous_swap_ids).await;

    (xmr_address_wallet_name, btc_address, swap_id)
}

#[allow(clippy::too_many_arguments)]
async fn run_swaps_parallel(
    swap_info: HashMap<SwapId, SwapParams>,
    bitcoin_rpc: Arc<bitcoincore_rpc::Client>,
    monero_regtest: monero_rpc::RegtestDaemonClient,
    monero_wallet: Arc<Mutex<monero_rpc::WalletClient>>,
    execution_mutex: Arc<Mutex<u8>>,
) {
    bitcoin_rpc
        .generate_to_address(1, &reusable_btc_address())
        .unwrap();

    let lock = execution_mutex.lock().await;

    // run until bob has the btc funding address
    for (swap_id, SwapParams { data_dir_bob, .. }) in swap_info.iter() {
        let (address, amount) = retry_until_bitcoin_funding_address(
            swap_id.clone(),
            needs_funding_args(data_dir_bob.clone(), "bitcoin".to_string()),
        )
        .await;

        // fund the bitcoin address
        bitcoin_rpc
            .send_to_address(&address, amount, None, None, None, None, None, None)
            .unwrap();
    }

    for (swap_id, SwapParams { data_dir_alice, .. }) in swap_info.iter() {
        println!("waiting for AliceState(RefundSigs");
        retry_until_finish_state_transition(
            progress_args(data_dir_alice.clone(), swap_id.clone()),
            "AliceState(RefundSigs".to_string(),
        )
        .await;
    }

    // run until BobState(CoreArb) is received
    for (swap_id, SwapParams { data_dir_bob, .. }) in swap_info.iter() {
        println!("waiting for BobState(CoreArb)");
        retry_until_finish_state_transition(
            progress_args(data_dir_bob.clone(), swap_id.clone()),
            "BobState(CoreArb)".to_string(),
        )
        .await;
    }

    // run until the funding infos are cleared again
    for (swap_id, SwapParams { data_dir_bob, .. }) in swap_info.iter() {
        println!("waiting for the funding info to clear");
        retry_until_funding_info_cleared(
            swap_id.clone(),
            needs_funding_args(data_dir_bob.clone(), "bitcoin".to_string()),
        )
        .await;
    }

    tokio::time::sleep(time::Duration::from_secs(20)).await;

    // generate some bitcoin blocks to finalize the bitcoin arb lock tx
    bitcoin_rpc
        .generate_to_address(3, &reusable_btc_address())
        .unwrap();

    // run until the alice has the monero funding address
    for (swap_id, SwapParams { data_dir_alice, .. }) in swap_info.iter() {
        let (monero_address, monero_amount) = retry_until_monero_funding_address(
            swap_id.clone(),
            needs_funding_args(data_dir_alice.clone(), "monero".to_string()),
        )
        .await;
        send_monero(Arc::clone(&monero_wallet), monero_address, monero_amount).await;
    }

    // run until the funding infos are cleared again
    for (swap_id, SwapParams { data_dir_alice, .. }) in swap_info.iter() {
        println!("waiting for the funding info to clear");
        retry_until_funding_info_cleared(
            swap_id.clone(),
            needs_funding_args(data_dir_alice.clone(), "monero".to_string()),
        )
        .await;
    }

    // generate some monero blocks to finalize the monero acc lock tx
    monero_regtest
        .generate_blocks(6, reusable_xmr_address())
        .await
        .unwrap();

    // run until BobState(BuySig) is received
    for (swap_id, SwapParams { data_dir_bob, .. }) in swap_info.iter() {
        println!("waiting for BobState(BuySig)");
        retry_until_finish_state_transition(
            progress_args(data_dir_bob.clone(), swap_id.clone()),
            "BobState(BuySig)".to_string(),
        )
        .await;
    }

    tokio::time::sleep(time::Duration::from_secs(10)).await;

    // generate some bitcoin blocks to make the buy tx final
    bitcoin_rpc
        .generate_to_address(5, &reusable_btc_address())
        .unwrap();

    // run until the AliceState(Finish) is received
    for (swap_id, SwapParams { data_dir_alice, .. }) in swap_info.iter() {
        retry_until_finish_state_transition(
            progress_args(data_dir_alice.clone(), swap_id.clone()),
            "AliceState(Finish(Success(Swapped)))".to_string(),
        )
        .await;
    }

    // generate some blocks on bitcoin's side
    bitcoin_rpc
        .generate_to_address(2, &reusable_btc_address())
        .unwrap();

    // check that btc was received in the destination address
    for (
        _,
        SwapParams {
            destination_btc_address,
            ..
        },
    ) in swap_info.iter()
    {
        let balance = bitcoin_rpc
            .get_received_by_address(&destination_btc_address, None)
            .unwrap();
        assert!(balance.as_sat() > 90000000);
    }

    // cache the monero balance before sweeping
    let mut before_balances: HashMap<SwapId, u64> = HashMap::new();
    for (
        swap_id,
        SwapParams {
            xmr_dest_wallet_name,
            ..
        },
    ) in swap_info.iter()
    {
        let monero_wallet_lock = monero_wallet.lock().await;
        monero_wallet_lock
            .open_wallet(xmr_dest_wallet_name.clone(), None)
            .await
            .unwrap();
        before_balances.insert(
            *swap_id,
            monero_wallet_lock
                .get_balance(0, None)
                .await
                .unwrap()
                .balance,
        );
        drop(monero_wallet_lock);
    }

    // Sleep here to work around a race condition between pending
    // SweepXmrAddress requests and tx Acc Lock confirmations. If Acc Lock
    // confirmations are produced before the pending request is queued, no
    // action will take place after this point.
    tokio::time::sleep(time::Duration::from_secs(20)).await;

    // generate some blocks on monero's side
    monero_regtest
        .generate_blocks(10, reusable_xmr_address())
        .await
        .unwrap();

    // run until the BobState(Finish) is received
    for (swap_id, SwapParams { data_dir_bob, .. }) in swap_info.iter() {
        retry_until_finish_state_transition(
            progress_args(data_dir_bob.clone(), swap_id.clone()),
            "BobState(Finish(Success(Swapped)))".to_string(),
            // monero_regtest.clone(),
        )
        .await;
    }

    monero_regtest
        .generate_blocks(1, reusable_xmr_address())
        .await
        .unwrap();

    for (
        swap_id,
        SwapParams {
            xmr_dest_wallet_name,
            ..
        },
    ) in swap_info.iter()
    {
        let monero_wallet_lock = monero_wallet.lock().await;
        monero_wallet_lock
            .open_wallet(xmr_dest_wallet_name.clone(), None)
            .await
            .unwrap();
        monero_wallet_lock.refresh(Some(1)).await.unwrap();
        let after_balance = monero_wallet_lock.get_balance(0, None).await.unwrap();
        drop(monero_wallet_lock);
        let delta_balance = after_balance.balance - before_balances[swap_id];
        assert!(delta_balance > 999660000000);
    }
    drop(lock);
}

#[allow(clippy::too_many_arguments)]
async fn run_swap(
    swap_id: SwapId,
    data_dir_alice: Vec<String>,
    data_dir_bob: Vec<String>,
    bitcoin_rpc: Arc<bitcoincore_rpc::Client>,
    funding_btc_address: bitcoin::Address,
    monero_regtest: monero_rpc::RegtestDaemonClient,
    monero_wallet: Arc<Mutex<monero_rpc::WalletClient>>,
    monero_dest_wallet_name: String,
    execution_mutex: Arc<Mutex<u8>>,
) {
    let cli_alice_progress_args: Vec<String> =
        progress_args(data_dir_alice.clone(), swap_id.clone());
    let cli_bob_progress_args: Vec<String> = progress_args(data_dir_bob.clone(), swap_id.clone());
    let cli_bob_needs_funding_args: Vec<String> =
        needs_funding_args(data_dir_bob, "bitcoin".to_string());
    let cli_alice_needs_funding_args: Vec<String> =
        needs_funding_args(data_dir_alice, "monero".to_string());

    bitcoin_rpc
        .generate_to_address(1, &reusable_btc_address())
        .unwrap();

    let lock = execution_mutex.lock().await;

    // run until bob has the btc funding address
    let (address, amount) =
        retry_until_bitcoin_funding_address(swap_id.clone(), cli_bob_needs_funding_args.clone())
            .await;

    // fund the bitcoin address
    bitcoin_rpc
        .send_to_address(&address, amount, None, None, None, None, None, None)
        .unwrap();

    println!("waiting for AliceState(RefundSigs");
    retry_until_finish_state_transition(
        cli_alice_progress_args.clone(),
        "AliceState(RefundSigs".to_string(),
    )
    .await;

    // run until BobState(CoreArb) is received
    println!("waiting for BobState(CoreArb)");
    retry_until_finish_state_transition(
        cli_bob_progress_args.clone(),
        "BobState(CoreArb)".to_string(),
    )
    .await;

    // run until the funding infos are cleared again
    println!("waiting for the bitcoin funding info to clear");
    retry_until_funding_info_cleared(swap_id.clone(), cli_bob_needs_funding_args.clone()).await;

    tokio::time::sleep(time::Duration::from_secs(10)).await;

    // generate some bitcoin blocks to finalize the bitcoin arb lock tx
    bitcoin_rpc
        .generate_to_address(3, &reusable_btc_address())
        .unwrap();

    // run until the alice has the monero funding address
    let (monero_address, monero_amount) =
        retry_until_monero_funding_address(swap_id, cli_alice_needs_funding_args.clone()).await;
    send_monero(Arc::clone(&monero_wallet), monero_address, monero_amount).await;

    // run until the funding infos are cleared again
    println!("waiting for the monero funding info to clear");
    retry_until_funding_info_cleared(swap_id.clone(), cli_alice_needs_funding_args.clone()).await;

    // generate some monero blocks to finalize the monero acc lock tx
    monero_regtest
        .generate_blocks(6, reusable_xmr_address())
        .await
        .unwrap();

    // run until BobState(BuySig) is received
    retry_until_finish_state_transition(
        cli_bob_progress_args.clone(),
        "BobState(BuySig)".to_string(),
    )
    .await;

    tokio::time::sleep(time::Duration::from_secs(10)).await;

    // generate some bitcoin blocks to make the buy tx final
    bitcoin_rpc
        .generate_to_address(5, &reusable_btc_address())
        .unwrap();

    // run until the AliceState(Finish) is received
    retry_until_finish_state_transition(
        cli_alice_progress_args.clone(),
        "AliceState(Finish(Success(Swapped)))".to_string(),
    )
    .await;

    // generate some blocks on bitcoin's side
    bitcoin_rpc
        .generate_to_address(1, &reusable_btc_address())
        .unwrap();

    let (_stdout, _stderr) = run("../swap-cli", cli_bob_progress_args.clone()).unwrap();

    // check that btc was received in the destination address
    let balance = bitcoin_rpc
        .get_received_by_address(&funding_btc_address, None)
        .unwrap();
    assert!(balance.as_sat() > 90000000);

    // cache the monero balance before sweeping
    let monero_wallet_lock = monero_wallet.lock().await;
    monero_wallet_lock
        .open_wallet(monero_dest_wallet_name.clone(), None)
        .await
        .unwrap();
    let before_balance = monero_wallet_lock.get_balance(0, None).await.unwrap();
    drop(monero_wallet_lock);

    // Sleep here to work around a race condition between pending
    // SweepXmrAddress requests and tx Acc Lock confirmations. If Acc Lock
    // confirmations are produced before the pending request is queued, no
    // action will take place after this point.
    tokio::time::sleep(time::Duration::from_secs(10)).await;

    // generate some blocks on monero's side
    monero_regtest
        .generate_blocks(10, reusable_xmr_address())
        .await
        .unwrap();

    // run until the BobState(Finish) is received
    retry_until_bob_finish_state_transition(
        cli_bob_progress_args.clone(),
        "BobState(Finish(Success(Swapped)))".to_string(),
        monero_regtest.clone(),
    )
    .await;

    monero_regtest
        .generate_blocks(1, reusable_xmr_address())
        .await
        .unwrap();

    let monero_wallet_lock = monero_wallet.lock().await;
    monero_wallet_lock
        .open_wallet(monero_dest_wallet_name, None)
        .await
        .unwrap();
    monero_wallet_lock.refresh(Some(1)).await.unwrap();
    let after_balance = monero_wallet_lock.get_balance(0, None).await.unwrap();
    drop(monero_wallet_lock);
    drop(lock);
    let delta_balance = after_balance.balance - before_balance.balance;
    assert!(delta_balance > 999660000000);
}

fn cleanup_processes(mut farcasterds: Vec<process::Child>) {
    // clean up processes
    let sys = System::new_all();
    let procs: Vec<_> = sys
        .get_processes()
        .iter()
        .filter(|(_pid, process)| {
            ["farcasterd"].contains(&process.name())
                && farcasterds
                    .iter()
                    .map(|daemon| daemon.id())
                    .collect::<Vec<_>>()
                    .contains(&(process.parent().unwrap() as u32))
        })
        .collect();

    let procs_peerd: Vec<_> = sys
        .get_processes()
        .iter()
        .filter(|(_pid, process)| {
            ["peerd"].contains(&process.name())
                && procs
                    .iter()
                    .map(|proc| proc.0)
                    .collect::<Vec<_>>()
                    .contains(&&(process.parent().unwrap()))
        })
        .collect();

    let _procs: Vec<_> = sys
        .get_processes()
        .iter()
        .filter(|(_pid, process)| {
            ["peerd"].contains(&process.name())
                && procs_peerd
                    .iter()
                    .map(|proc| proc.0)
                    .collect::<Vec<_>>()
                    .contains(&&(process.parent().unwrap()))
        })
        .map(|(pid, _process)| {
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(*pid as i32),
                nix::sys::signal::Signal::SIGINT,
            )
            .expect("Sending CTRL-C failed")
        })
        .collect();

    let _procs: Vec<_> = sys
        .get_processes()
        .iter()
        .filter(|(_pid, process)| {
            ["swapd", "grpcd", "walletd", "syncerd", "peerd"].contains(&process.name())
                && procs
                    .iter()
                    .map(|proc| proc.0)
                    .collect::<Vec<_>>()
                    .contains(&&(process.parent().unwrap()))
        })
        .map(|(pid, _process)| {
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(*pid as i32),
                nix::sys::signal::Signal::SIGINT,
            )
            .expect("Sending CTRL-C failed")
        })
        .collect();

    procs.iter().for_each(|daemon| {
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(*daemon.0),
            nix::sys::signal::Signal::SIGINT,
        )
        .expect("Sending CTRL-C failed")
    });

    farcasterds
        .iter_mut()
        .for_each(|daemon| daemon.kill().expect("Couldn't kill farcasterd"));
}

fn reusable_btc_address() -> bitcoin::Address {
    bitcoin::Address::from_str("bcrt1q3rc4sm3w9fr6a46n08znfjt7eu2yhhel6j8rsa").unwrap()
}

fn reusable_xmr_address() -> monero::Address {
    monero::Address::from_str("44CpGC77Kn6exUWYCUwfaUYmDeKn7MyRcNPikgeHBCz8M6LXUC3fGCWNMW7UACHyTL6QxzqKxvJbu5o2VESLzCaeNHNUkwv").unwrap()
}

fn info_args(data_dir: Vec<String>) -> Vec<String> {
    data_dir
        .into_iter()
        .chain(vec!["info".to_string()])
        .collect()
}

fn farcasterd_args(data_dir: Vec<String>, server_args: Vec<&str>, extra: Vec<&str>) -> Vec<String> {
    data_dir
        .into_iter()
        .chain(server_args.into_iter().map(|s| s.into()))
        .chain(extra.into_iter().map(|s| s.into()))
        .collect()
}

fn make_offer_args(
    data_dir: Vec<String>,
    role: String,
    btc_addr: String,
    btc_amount: bitcoin::Amount,
    xmr_addr: String,
    xmr_amount: monero::Amount,
) -> Vec<String> {
    data_dir
        .into_iter()
        .chain(vec![
            "make".to_string(),
            "--btc-addr".to_string(),
            btc_addr,
            "--xmr-addr".to_string(),
            xmr_addr,
            "--network".to_string(),
            "Local".to_string(),
            "--arb-blockchain".to_string(),
            "ECDSA".to_string(),
            "--acc-blockchain".to_string(),
            "Monero".to_string(),
            "--btc-amount".to_string(),
            format!("{}", btc_amount),
            "--xmr-amount".to_string(),
            format!("{}", xmr_amount),
            "--maker-role".to_string(),
            role,
            "--cancel-timelock".to_string(),
            "10".to_string(),
            "--punish-timelock".to_string(),
            "30".to_string(),
            "--fee-strategy".to_string(),
            "1 satoshi/vByte".to_string(),
            "--public-ip-addr".to_string(),
            "127.0.0.1".to_string(),
            "--bind-ip-addr".to_string(),
            "0.0.0.0".to_string(),
            "--port".to_string(),
            "9376".to_string(),
        ])
        .collect()
}

fn take_offer_args(
    data_dir: Vec<String>,
    btc_addr: String,
    xmr_addr: String,
    offer: String,
) -> Vec<String> {
    data_dir
        .into_iter()
        .chain(vec![
            "take".to_string(),
            "--btc-addr".to_string(),
            btc_addr,
            "--xmr-addr".to_string(),
            xmr_addr,
            "--offer".to_string(),
            offer,
            "--without-validation".to_string(),
        ])
        .collect()
}

fn progress_args(data_dir: Vec<String>, swap_id: SwapId) -> Vec<String> {
    data_dir
        .into_iter()
        .chain(vec!["progress".to_string(), format!("{:#?}", swap_id)])
        .collect()
}

fn needs_funding_args(data_dir: Vec<String>, currency: String) -> Vec<String> {
    data_dir
        .into_iter()
        .chain(vec!["needs-funding".to_string(), currency])
        .collect()
}

fn cli_output_to_node_info(stdout: Vec<String>) -> NodeInfo {
    serde_yaml::from_str(
        &stdout
            .iter()
            .map(|line| format!("{}{}", line, "\n"))
            .collect::<String>(),
    )
    .unwrap()
}

async fn retry_until_offer(args: Vec<String>) -> Vec<String> {
    for _ in 0..ALLOWED_RETRIES {
        let (stdout, _stderr) = run("../swap-cli", args.clone()).unwrap();
        let offers: Vec<String> = cli_output_to_node_info(stdout)
            .offers
            .iter()
            .map(|offer| offer.to_string())
            .collect();
        if !offers.is_empty() {
            return offers;
        }
        tokio::time::sleep(time::Duration::from_secs(1)).await;
    }
    panic!("timeout before any offer could be retrieved");
}

async fn retry_until_offer_parallel(
    args: Vec<String>,
    previous_offers: Arc<Mutex<HashSet<String>>>,
) -> String {
    for _ in 0..ALLOWED_RETRIES {
        let mut previous_offers_lock = previous_offers.lock().await;
        let (stdout, _stderr) = run("../swap-cli", args.clone()).unwrap();
        let new_offers: HashSet<String> = cli_output_to_node_info(stdout)
            .offers
            .drain(..)
            .map(|offer| offer.to_string())
            .collect();
        if let Some(offer) = new_offers.difference(&previous_offers_lock.clone()).next() {
            previous_offers_lock.insert(offer.clone());
            drop(previous_offers_lock);
            return offer.clone();
        }
        tokio::time::sleep(time::Duration::from_secs(1)).await;
    }
    panic!("timeout before any offer could be retrieved");
}

async fn retry_until_swap_id(args: Vec<String>, previous_swap_ids: HashSet<SwapId>) -> SwapId {
    for _ in 0..ALLOWED_RETRIES {
        let (stdout, _stderr) = run("../swap-cli", args.clone()).unwrap();
        let new_swap_ids: HashSet<SwapId> =
            cli_output_to_node_info(stdout).swaps.drain(..).collect();
        if let Some(swap_id) = new_swap_ids.difference(&previous_swap_ids).next() {
            return swap_id.clone();
        }
        tokio::time::sleep(time::Duration::from_secs(1)).await;
    }
    panic!("timeout before any swapid could be retrieved");
}

async fn retry_until_swap_id_parallel(
    args: Vec<String>,
    previous_swap_ids: Arc<Mutex<HashSet<SwapId>>>,
) -> SwapId {
    for _ in 0..ALLOWED_RETRIES {
        let mut previous_swap_ids_lock = previous_swap_ids.lock().await;
        let (stdout, _stderr) = run("../swap-cli", args.clone()).unwrap();
        let new_swap_ids: HashSet<SwapId> =
            cli_output_to_node_info(stdout).swaps.drain(..).collect();

        if let Some(swap_id) = new_swap_ids
            .difference(&previous_swap_ids_lock.clone())
            .next()
        {
            previous_swap_ids_lock.insert(swap_id.clone());
            drop(previous_swap_ids_lock);
            return swap_id.clone();
        }
        tokio::time::sleep(time::Duration::from_secs(1)).await;
    }
    panic!("timeout before any swapid could be retrieved");
}

async fn retry_until_bitcoin_funding_address(
    swap_id: SwapId,
    args: Vec<String>,
) -> (bitcoin::Address, bitcoin::Amount) {
    for _ in 0..ALLOWED_RETRIES {
        let (stdout, _stderr) = run("../swap-cli", args.clone()).unwrap();

        // get the btc funding address
        let funding_infos: Vec<BitcoinFundingInfo> = stdout
            .iter()
            .filter_map(|element| {
                println!("element: {:?}", element);
                if let Ok(funding_info) = BitcoinFundingInfo::from_str(element) {
                    if funding_info.swap_id == swap_id {
                        Some(funding_info)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();

        if !funding_infos.is_empty() {
            return (funding_infos[0].address.clone(), funding_infos[0].amount);
        }
        tokio::time::sleep(time::Duration::from_secs(1)).await;
    }
    panic!("timeout before any bitcoin funding address could be retrieved");
}

async fn retry_until_funding_info_cleared(swap_id: SwapId, args: Vec<String>) {
    for _ in 0..ALLOWED_RETRIES {
        let (stdout, _stderr) = run("../swap-cli", args.clone()).unwrap();

        // get the btc funding address
        let funding_infos: Vec<BitcoinFundingInfo> = stdout
            .iter()
            .filter_map(|element| {
                println!("element: {:?}", element);
                if let Ok(funding_info) = BitcoinFundingInfo::from_str(element) {
                    if funding_info.swap_id == swap_id {
                        Some(funding_info)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();

        if funding_infos.is_empty() {
            return;
        }
        tokio::time::sleep(time::Duration::from_secs(1)).await;
    }
    panic!("timeout before any bitcoin funding address could be retrieved");
}

async fn retry_until_monero_funding_address(
    swap_id: SwapId,
    args: Vec<String>,
) -> (monero::Address, monero::Amount) {
    for _ in 0..ALLOWED_RETRIES {
        let (stdout, _stderr) = run("../swap-cli", args.clone()).unwrap();

        // get the monero funding address
        let funding_infos: Vec<MoneroFundingInfo> = stdout
            .iter()
            .filter_map(|element| {
                if let Ok(funding_info) = MoneroFundingInfo::from_str(element) {
                    if funding_info.swap_id == swap_id {
                        Some(funding_info)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect();

        if !funding_infos.is_empty() {
            return (funding_infos[0].address.clone(), funding_infos[0].amount);
        }
        tokio::time::sleep(time::Duration::from_secs(1)).await;
    }
    panic!("timeout before any monero funding address could be retrieved");
}

async fn retry_until_bob_finish_state_transition(
    args: Vec<String>,
    finish_state: String,
    monero_regtest: monero_rpc::RegtestDaemonClient,
) -> Vec<String> {
    for _ in 0..ALLOWED_RETRIES {
        let (stdout, _stderr) = run("../swap-cli", args.clone()).unwrap();

        let bob_finish: Vec<String> = stdout
            .iter()
            .filter_map(|element| {
                if element.contains(&finish_state) {
                    Some(element.to_string())
                } else {
                    None
                }
            })
            .collect();

        if !bob_finish.is_empty() {
            return bob_finish;
        }

        monero_regtest
            .generate_blocks(1, reusable_xmr_address())
            .await
            .unwrap();

        tokio::time::sleep(time::Duration::from_secs(1)).await;
    }
    panic!(
        "timeout before finish state {:?} could be retrieved",
        finish_state
    );
}

async fn retry_until_finish_state_transition(
    args: Vec<String>,
    finish_state: String,
) -> Vec<String> {
    for _ in 0..ALLOWED_RETRIES {
        let (stdout, _stderr) = run("../swap-cli", args.clone()).unwrap();

        let alice_finish: Vec<String> = stdout
            .iter()
            .filter_map(|element| {
                println!("element: {:?}", element);
                if element.contains(&finish_state) {
                    Some(element.to_string())
                } else {
                    None
                }
            })
            .collect();

        if !alice_finish.is_empty() {
            return alice_finish;
        }

        tokio::time::sleep(time::Duration::from_secs(1)).await;
    }
    panic!(
        "timeout before finish state {:?} could be retrieved",
        finish_state
    );
}

fn bitcoin_setup() -> bitcoincore_rpc::Client {
    let conf = config::TestConfig::parse();
    let bitcoin_rpc =
        Client::new(&format!("{}", conf.bitcoin.daemon), conf.bitcoin.get_auth()).unwrap();

    // make sure a wallet is created and loaded
    if bitcoin_rpc
        .create_wallet("wallet", None, None, None, None)
        .is_err()
    {
        let _ = bitcoin_rpc.load_wallet("wallet");
    }

    let address = bitcoin_rpc.get_new_address(None, None).unwrap();
    bitcoin_rpc.generate_to_address(200, &address).unwrap();
    bitcoin_rpc
}

async fn monero_setup() -> (
    monero_rpc::RegtestDaemonClient,
    Arc<Mutex<monero_rpc::WalletClient>>,
) {
    let conf = config::TestConfig::parse();
    let client = monero_rpc::RpcClient::new(format!("{}", conf.monero.daemon));
    let regtest = client.daemon().regtest();
    let client = monero_rpc::RpcClient::new(format!(
        "{}",
        conf.monero.get_wallet(config::WalletIndex::Primary)
    ));
    let wallet = client.wallet();

    if wallet.open_wallet("test".to_string(), None).await.is_err() {
        // TODO: investigate this error in monero-rpc-rs
        if wallet
            .create_wallet("test".to_string(), None, "English".to_string())
            .await
            .is_err()
        {};
        wallet.open_wallet("test".to_string(), None).await.unwrap();
    }

    let address = wallet.get_address(0, None).await.unwrap();
    regtest.generate_blocks(200, address.address).await.unwrap();
    regtest
        .generate_blocks(200, reusable_xmr_address())
        .await
        .unwrap();

    (regtest, Arc::new(Mutex::new(wallet)))
}

async fn monero_new_dest_address(
    wallet: Arc<Mutex<monero_rpc::WalletClient>>,
) -> (monero::Address, String) {
    use rand::distributions::Alphanumeric;
    use rand::{thread_rng, Rng};

    let wallet_name: String = thread_rng()
        .sample_iter(&Alphanumeric)
        .take(15)
        .map(char::from)
        .collect();

    let wallet_lock = wallet.lock().await;

    // TODO: investigate this error in monero-rpc-rs
    if wallet_lock
        .create_wallet(wallet_name.clone(), None, "English".to_string())
        .await
        .is_err()
    {};
    wallet_lock
        .open_wallet(wallet_name.clone(), None)
        .await
        .unwrap();
    let address = wallet_lock.get_address(0, None).await.unwrap();

    (address.address, wallet_name)
}

pub fn run(
    name: &str,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> io::Result<(Vec<String>, Vec<String>)> {
    let mut bin_path = std::env::current_exe().map_err(|err| {
        error!("Unable to detect binary directory: {}", err);
        err
    })?;
    bin_path.pop();

    bin_path.push(name);
    #[cfg(target_os = "windows")]
    bin_path.set_extension("exe");

    debug!(
        "Launching {} as a separate process using `{}` as binary",
        name,
        bin_path.to_string_lossy()
    );

    let res = process::Command::new(bin_path)
        .args(args)
        .output()
        .expect("failed to run command");

    let stdout = String::from_utf8_lossy(&res.stdout)
        .to_string()
        .lines()
        .map(|line| {
            let plain_bytes = strip_ansi_escapes::strip(&line).unwrap();
            str::from_utf8(&plain_bytes).unwrap().to_string()
        })
        .collect();
    let stderr = String::from_utf8_lossy(&res.stderr)
        .to_string()
        .lines()
        .map(|line| {
            let plain_bytes = strip_ansi_escapes::strip(&line).unwrap();
            str::from_utf8(&plain_bytes).unwrap().to_string()
        })
        .collect();
    Ok((stdout, stderr))
}

pub fn launch(name: &str, args: impl IntoIterator<Item = String>) -> io::Result<process::Child> {
    let mut bin_path = std::env::current_exe().map_err(|err| {
        error!("Unable to detect binary directory: {}", err);
        err
    })?;
    bin_path.pop();
    bin_path.push(name);

    println!(
        "Launching {} as a separate process using `{}` as binary",
        name,
        bin_path.to_string_lossy()
    );

    let cmdargs = args.into_iter().collect::<Vec<String>>().join(" ");
    println!("Command arguments: \"{}\"", cmdargs);

    let mut shell = process::Command::new("sh");
    shell
        .arg("-c")
        .arg(format!("{} {}", bin_path.to_string_lossy(), cmdargs));

    println!("Executing `{:?}`", shell);
    shell.spawn().map_err(|err| {
        error!("Error launching {}: {}", name, err);
        err
    })
}

async fn send_monero(
    wallet: Arc<Mutex<monero_rpc::WalletClient>>,
    address: monero::Address,
    amount: monero::Amount,
) -> Vec<u8> {
    let options = monero_rpc::TransferOptions {
        account_index: None,
        subaddr_indices: None,
        mixin: None,
        ring_size: None,
        unlock_time: None,
        payment_id: None,
        do_not_relay: None,
    };
    let wallet_lock = wallet.lock().await;
    wallet_lock
        .open_wallet("test".to_string(), None)
        .await
        .unwrap();
    wallet_lock.refresh(Some(1)).await.unwrap();
    let transaction = wallet_lock
        .transfer(
            [(address, amount)].iter().cloned().collect(),
            monero_rpc::TransferPriority::Default,
            options.clone(),
        )
        .await
        .unwrap();
    drop(wallet_lock);
    hex::decode(transaction.tx_hash.to_string()).unwrap()
}
