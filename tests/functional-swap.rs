#[macro_use]
extern crate log;

use clap::Clap;
use bitcoincore_rpc::{Auth, Client, RpcApi};
use farcaster_node::rpc::Client as FarcasterClient;
use farcaster_node::{Config, LogStyle};
use sysinfo::{ProcessExt, System, SystemExt};
use std::process;
use std::{ffi::OsStr};
use std::io;
use std::{thread, time};

use microservices::shell::Exec;
use std::str::FromStr;
use farcaster_node::cli::Opts;


#[tokio::test]
async fn swap_test() {
    // let res = process::Command::new("ls").output().expect("failed to execute swap-cli help");
    // println!("result: {:?}", res);
    // let res = process::Command::new("ls").arg("..").output().expect("failed to execute swap-cli help");
    // println!("result: {:?}", res);

    // electrum_server: "tcp://localhost:50001".to_string(),
    // monero_daemon: "http://localhost:18081".to_string(),
    // monero_rpc_wallet: "http://localhost:18084".to_string(),
    // ./target/debug/farcasterd -vv -d .data_dir_1 
    // --electrum-server localhost:60001 
    // --monero-daemon http://localhost:38081 --monero-rpc-wallet http://localhost:18083

    let bitcoin_rpc = bitcoin_setup();
    let btc_address = bitcoin_rpc.get_new_address(None, None).unwrap();

    let (monero_regtest, monero_wallet) = monero_setup().await;
    let xmr_address = monero_wallet.get_address(0, None).await.unwrap();

    // data directors
    let data_dir_maker: Vec<&str> = vec!["-d", "tests/.farcaster_node_0"];
    let data_dir_taker = vec!["-d", "tests/.farcaster_node_1"];
    // destination addresses
    let btc_addr = btc_address.clone().to_string();
    let xmr_addr = xmr_address.address.clone().to_string();
    // create config from opts
    let mut opts = Opts::parse_from(
        vec!["swap-cli"]
            .into_iter()
            .chain(data_dir_maker.clone())
            .chain(vec!["make", &btc_addr, &xmr_addr]),
    );
    opts.process();
    info!("opts: {:?}", opts);
    let config: Config = opts.shared.clone().into();

    // run("../swap-cli", vec!["help"]).expect("should always be ok");

    let mut client =
        FarcasterClient::with(config.clone(), config.chain.clone()).expect("Error running client");

    let server_args = vec![
        "--electrum-server", "localhost:50001",
        "--monero-daemon", "http://localhost:18081",
        "--monero-rpc-wallet", "http://localhost:18084",
    ];
    // let maker_args: Vec<String> = data_dir_maker.clone().extend(server_args.clone());
    // let taker_args = data_dir_taker.clone().append(&mut server_args.clone());
    let maker_args = vec![].into_iter().chain(data_dir_maker.clone()).chain(server_args.clone());
    let taker_args = vec![].into_iter().chain(data_dir_taker.clone()).chain(server_args.clone());

    let mut farcasterd_maker = launch("../farcasterd", maker_args).unwrap();
    let mut farcasterd_taker = launch("../farcasterd", taker_args).unwrap();

    let maker_info_args = vec![].into_iter().chain(data_dir_maker.clone()).chain(vec!["info"]);
    let taker_info_args = vec![].into_iter().chain(data_dir_taker.clone()).chain(vec!["info"]);
    let (stdout, stderr) = run("../swap-cli", maker_info_args.clone()).expect("should always be ok");
    println!("stdout: {:?}", stdout);
    println!("stderr: {:?}", stderr);

    // Command::Ls
    //     .exec(&mut client)
    //     .unwrap_or_else(|err| eprintln!("{} {}", "error:".err(), err.err()));

    info!("executing command: {:?}", opts.command);
    opts.command
        .exec(&mut client)
        .unwrap_or_else(|err| eprintln!("{} {}", "error:".err(), err.err()));

    // set up maker

    // make tb1q4gj53tuew3e6u4a32kdtle2q72su8te39dpceq
    // 55LTR8KniP4LQGJSPtbYDacR7dz8RBFnsfAKMaMuwUNYX6aQbBcovzDPyrQF9KXF9tVU6Xk3K8no1BywnJX6GvZX8yJsXvt
    // Testnet ECDSA Monero "0.00001350 BTC" "0.00000001 XMR" Alice 10 30 "1
    // satoshi/vByte" "127.0.0.1" "0.0.0.0" 9745

    let maker_args =  vec![
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
    ];

    let mut opts = Opts::parse_from(
        vec!["swap-cli"]
            .into_iter()
            .chain(data_dir_maker.clone())
            .chain(maker_args),
    );
    let res = opts.command
        .exec(&mut client)
        .unwrap_or_else(|err| eprintln!("{} {}", "error:".err(), err.err()));

    let (stdout, stderr) = run("../swap-cli", maker_info_args).expect("should always be ok");
    println!("stdout: {:?}", stdout);
    println!("stderr: {:?}", stderr);

    let offers: Vec<String> = stdout.iter().filter_map(|element| {
        if element.to_string().len() > 5 {
            let pos = element.find("Offer");
            if pos.is_some() {
                let pos = pos.unwrap();
                let len = element.to_string().len()-1;
                let offer = element[pos..len].to_string();
                return Some(offer);
            }
        }
        None
    }).collect();
    println!("offers: {:?}", offers);

    // set up taker

    opts = Opts::parse_from(
        vec!["swap-cli"]
            .into_iter()
            .chain(data_dir_taker.clone())
            .chain(vec![
                "take", 
                "tb1q4gj53tuew3e6u4a32kdtle2q72su8te39dpceq", 
                "55LTR8KniP4LQGJSPtbYDacR7dz8RBFnsfAKMaMuwUNYX6aQbBcovzDPyrQF9KXF9tVU6Xk3K8no1BywnJX6GvZX8yJsXvt", 
                &offers[1],
                "--without-validation",
            ]),
    );
    opts.command
        .exec(&mut client)
        .unwrap_or_else(|err| eprintln!("{} {}", "error:".err(), err.err()));

    // clean up processes

    thread::sleep(time::Duration::from_secs(20));

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

    let res = process::Command::new(bin_path).args(args).output().expect("failed to run command");
    // println!("result: {:?}", res);
    // let stderr: Vec<String> = stderr.lines().map(|line| line.to_string()).collect();

    let stdout = String::from_utf8_lossy(&res.stdout).to_string().lines().map(|line| line.to_string()).collect();
    let stderr = String::from_utf8_lossy(&res.stderr).to_string().lines().map(|line| line.to_string()).collect();
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
