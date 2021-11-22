#[macro_use]
extern crate log;

use bitcoincore_rpc::{Auth, Client, RpcApi};
use clap::Clap;
use farcaster_node::rpc::Client as FarcasterClient;
use farcaster_node::{Config, LogStyle};
use std::ffi::OsStr;
use std::io;
use std::process;
use std::{thread, time};
use sysinfo::{ProcessExt, System, SystemExt};

use farcaster_node::cli::Opts;
use microservices::shell::Exec;
use std::str::FromStr;

#[tokio::test]
async fn swap_test() {
    let bitcoin_rpc = bitcoin_setup();
    let btc_address = bitcoin_rpc.get_new_address(None, None).unwrap();

    let (_monero_regtest, monero_wallet) = monero_setup().await;
    let xmr_address = monero_wallet.get_address(0, None).await.unwrap();

    // data directors
    let data_dir_maker = vec!["-d", "tests/.farcaster_node_0"];
    let data_dir_taker = vec!["-d", "tests/.farcaster_node_1"];
    // destination addresses
    let btc_addr = btc_address.clone().to_string();
    let xmr_addr = xmr_address.address.clone().to_string();

    let server_args = vec![
        "--electrum-server",
        "localhost:50001",
        "--monero-daemon",
        "http://localhost:18081",
        "--monero-rpc-wallet",
        "http://localhost:18084",
    ];
    let farcasterd_maker_args = vec![]
        .into_iter()
        .chain(data_dir_maker.clone())
        .chain(server_args.clone());
    let farcasterd_taker_args = vec![]
        .into_iter()
        .chain(data_dir_taker.clone())
        .chain(server_args.clone());

    let mut farcasterd_maker = launch("../farcasterd", farcasterd_maker_args).unwrap();
    let mut farcasterd_taker = launch("../farcasterd", farcasterd_taker_args).unwrap();

    let maker_info_args = vec![]
        .into_iter()
        .chain(data_dir_maker.clone())
        .chain(vec!["info"]);
    let taker_info_args = vec![]
        .into_iter()
        .chain(data_dir_taker.clone())
        .chain(vec!["info"]);
    let (stdout, stderr) =
        run("../swap-cli", maker_info_args.clone()).expect("should always be ok");
    println!("stdout: {:?}", stdout);
    println!("stderr: {:?}", stderr);

    // make an offer
    let cli_make_args = data_dir_maker.into_iter().chain(vec![
        "make",
        &btc_addr,
        &xmr_addr,
        "Testnet",
        "ECDSA",
        "Monero",
        "1 BTC",
        "1 XMR",
        "Alice",
        "10",
        "30",
        "1 satoshi/vByte",
        "127.0.0.1",
        "0.0.0.0",
        "9376",
    ]);

    let (stdout, stderr) = run("../swap-cli", cli_make_args).unwrap();
    println!("stdout: {:?}", stdout);
    println!("stderr: {:?}", stderr);

    // get offer strings
    let (stdout, _stderr) = run("../swap-cli", maker_info_args.clone()).unwrap();
    let offers: Vec<String> = stdout
        .iter()
        .filter_map(|element| {
            if element.to_string().len() > 5 {
                let pos = element.find("Offer");
                if pos.is_some() {
                    let pos = pos.unwrap();
                    let len = element.to_string().len() - 1;
                    let offer = element[pos..len].to_string();
                    return Some(offer);
                }
            }
            None
        })
        .collect();
    println!("offers: {:?}", offers);

    // take the offer
    let cli_take_args = data_dir_taker.clone().into_iter().chain(vec![
        "take",
        &btc_addr,
        &xmr_addr,
        &offers[0],
        "--without-validation",
    ]);
    let (stdout, stderr) = run("../swap-cli", cli_take_args).unwrap();
    println!("stdout: {:?}", stdout);
    println!("stderr: {:?}", stderr);

    thread::sleep(time::Duration::from_secs(30));

    let (stdout, stderr) = run("../swap-cli", maker_info_args).unwrap();
    println!("stdout: {:?}", stdout);
    println!("stderr: {:?}", stderr);
    let (stdout, stderr) = run("../swap-cli", taker_info_args).unwrap();
    println!("stdout: {:?}", stdout);
    println!("stderr: {:?}", stderr);

    // get the swap id
    let swap_ids: Vec<String> = stdout.iter().filter_map(|element| {
        if element.to_string().len() > 5 {
            let pos = element.find("\"0x");
            if pos.is_some() {
                let pos = pos.unwrap();
                let len = element.to_string().len() -1;
                let swap_id = element[pos+1..len].to_string();
                return Some(swap_id);
            }
        }
        None
    }).collect();
    println!("swap_id: {:?}", swap_ids);

    let cli_taker_progress_args = data_dir_taker.into_iter().chain(vec![
        "progress",
        &swap_ids[0],
    ]);
    let (stdout, stderr) = run("../swap-cli", cli_taker_progress_args).unwrap();
    println!("stdout: {:?}", stdout);
    println!("stderr: {:?}", stderr);

    // get the btc funding address
    let funding_address: Vec<String> = stdout.iter().filter_map(|element| {
        println!("element: {:?}", element);
        if element.to_string().len() > 5 {
            let pos = element.find("tb1");
            let end_pos = element.find("\\u{1b}");
            println!("element: {:?}", element);
            if pos.is_some() && end_pos.is_some() {
                let pos = pos.unwrap();
                let end_pos = end_pos.unwrap() - 1;
                let swap_id = element[pos..end_pos].to_string();
                return Some(swap_id);
            }
        }
        None
    }).collect();
    println!("funding address: {:?}", funding_address);

    // clean up processes
    thread::sleep(time::Duration::from_secs(2));
    let _procs: Vec<_> = System::new_all()
        .get_processes()
        .iter()
        .filter(|(_pid, process)| {
            ["peerd", "swapd", "walletd"].contains(&process.name())
                && [farcasterd_maker.id(), farcasterd_taker.id()]
                    .contains(&(process.parent().unwrap() as u32))
        })
        .map(|(pid, _process)| {
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(*pid as i32),
                nix::sys::signal::Signal::SIGINT,
            )
            .expect("Sending CTRL-C failed")
        })
        .collect();

    farcasterd_maker
        .kill()
        .expect("Couldn't kill farcasterd maker");
    farcasterd_taker
        .kill()
        .expect("Couldn't kill farcasterd taker");
}

fn bitcoin_setup() -> bitcoincore_rpc::Client {
    let path = std::path::PathBuf::from_str("tests/data_dir/regtest/.cookie").unwrap();
    let bitcoin_rpc = Client::new("http://localhost:18443", Auth::CookieFile(path)).unwrap();

    // make sure a wallet is created and loaded
    match bitcoin_rpc.create_wallet("wallet", None, None, None, None) {
        Err(_e) => match bitcoin_rpc.load_wallet("wallet") {
            _ => {}
        },
        _ => {}
    }
    let address = bitcoin_rpc.get_new_address(None, None).unwrap();
    bitcoin_rpc.generate_to_address(100, &address).unwrap();
    bitcoin_rpc
}

async fn monero_setup() -> (monero_rpc::RegtestDaemonClient, monero_rpc::WalletClient) {
    let daemon_client = monero_rpc::RpcClient::new("http://localhost:18081".to_string());
    let daemon = daemon_client.daemon();
    let regtest = daemon.regtest();
    let wallet_client = monero_rpc::RpcClient::new("http://localhost:18083".to_string());
    let wallet = wallet_client.wallet();
    match wallet
        .create_wallet("test".to_string(), None, "English".to_string())
        .await
    {
        _ => {
            wallet.open_wallet("test".to_string(), None).await.unwrap();
        }
    }
    let address = wallet.get_address(0, None).await.unwrap();
    regtest.generate_blocks(200, address.address).await.unwrap();

    (regtest, wallet)
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
    // println!("result: {:?}", res);
    // let stderr: Vec<String> = stderr.lines().map(|line| line.to_string()).collect();

    let stdout = String::from_utf8_lossy(&res.stdout)
        .to_string()
        .lines()
        .map(|line| line.to_string())
        .collect();
    let stderr = String::from_utf8_lossy(&res.stderr)
        .to_string()
        .lines()
        .map(|line| line.to_string())
        .collect();
    Ok((stdout, stderr))
}

pub fn launch(
    name: &str,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> io::Result<process::Child> {
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

    let mut cmd = process::Command::new(bin_path);

    cmd.args(args);

    trace!("Executing `{:?}`", cmd);
    cmd.spawn().map_err(|err| {
        error!("Error launching {}: {}", name, err);
        err
    })
}
