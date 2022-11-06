//! Helper functions to launch, control, and clean farcaster instances.

use std::env;
use std::ffi::OsStr;
use std::io;
use std::process;
use std::str;
use std::str::FromStr;
use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

use nix::unistd::{getpgid, getsid, Pid};
use serde_crate::de::DeserializeOwned;
use sysinfo::{ProcessExt, System, SystemExt};
use tokio::sync::Mutex;

use super::config;

// TODO: rename this function, this launches fcd, not 'clients'
pub async fn setup_clients() -> (process::Child, Vec<String>, process::Child, Vec<String>) {
    let (farcasterd_maker, data_dir_maker) = launch_farcasterd_maker();
    let (farcasterd_taker, data_dir_taker) = launch_farcasterd_taker();
    (
        farcasterd_maker,
        data_dir_maker,
        farcasterd_taker,
        data_dir_taker,
    )
}

// TODO: rename this function, this launches fcd, not 'clients'
pub fn launch_farcasterd_maker() -> (process::Child, Vec<String>) {
    // data directories
    let data_dir_maker = vec!["-d".to_string(), "tests/fc1".to_string()];

    // If we are in CI we use .ci.toml files, otherwise .toml
    let ctx = env::var("CI").unwrap_or_else(|_| "false".into());
    let ext = if ctx == "false" { ".toml" } else { ".ci.toml" };

    let farcasterd_maker_args = farcasterd_args(
        data_dir_maker.clone(),
        vec!["--config", &format!("tests/cfg/fc1{}", ext)],
        vec![],
    );

    let farcasterd_maker = launch("../farcasterd", farcasterd_maker_args).unwrap();
    (farcasterd_maker, data_dir_maker)
}

// TODO: rename this function, this launches fcd, not 'clients'
pub fn launch_farcasterd_taker() -> (process::Child, Vec<String>) {
    // data directories
    let data_dir_taker = vec!["-d".to_string(), "tests/fc2".to_string()];

    // If we are in CI we use .ci.toml files, otherwise .toml
    let ctx = env::var("CI").unwrap_or_else(|_| "false".into());
    let ext = if ctx == "false" { ".toml" } else { ".ci.toml" };

    let farcasterd_taker_args = farcasterd_args(
        data_dir_taker.clone(),
        vec!["--config", &format!("tests/cfg/fc2{}", ext)],
        vec![],
    );

    let farcasterd_taker = launch("../farcasterd", farcasterd_taker_args).unwrap();
    (farcasterd_taker, data_dir_taker)
}

fn farcasterd_args(data_dir: Vec<String>, server_args: Vec<&str>, extra: Vec<&str>) -> Vec<String> {
    data_dir
        .into_iter()
        .chain(server_args.into_iter().map(|s| s.into()))
        .chain(extra.into_iter().map(|s| s.into()))
        .collect()
}

fn launch(
    name: &str,
    args: impl IntoIterator<Item = String> + Clone,
) -> io::Result<process::Child> {
    let mut bin_path = std::env::current_exe().map_err(|err| {
        error!("Unable to detect binary directory: {}", err);
        err
    })?;
    bin_path.pop();
    bin_path.push(name);

    debug!(
        "Launching {} as a separate process using `{}` as binary",
        name,
        bin_path.to_string_lossy()
    );

    let cmdargs = args.clone().into_iter().collect::<Vec<String>>().join(" ");
    debug!("Command arguments: \"{}\"", cmdargs);

    let mut cmd = process::Command::new(bin_path.to_string_lossy().to_string());
    cmd.args(args);

    trace!("Executing `{:?}`", cmd);
    let child = cmd.spawn().map_err(|err| {
        error!("Error launching {}: {}", name, err);
        err
    })?;
    let pid = Pid::from_raw(child.id() as i32);
    trace!("pid got at launch: {:?}", pid);
    Ok(child)
}

pub fn cleanup_processes(farcasterds: Vec<process::Child>) {
    let pid = nix::unistd::getpid();
    let current_sid = getsid(Some(pid)).expect("Fail to get session id");
    trace!("current sid: {:?}", current_sid);

    for child in farcasterds {
        let sid = getsid(Some(Pid::from_raw(child.id() as i32))).expect("Fail to get session id");
        let gid = nix::unistd::getpgid(Some(Pid::from_raw(child.id() as i32)))
            .expect("Fail to get process group id");
        trace!("Killing child id: {:?}, ", child.id());
        trace!("sid: {:?}", sid);
        trace!("pgid: {:?}", gid);

        trace!("egid: {:?}", nix::unistd::getegid());
        trace!("euid: {:?}", nix::unistd::geteuid());

        // log the process tree
        let mut system = System::new();
        system.refresh_all();
        for (pid_i32, process) in system.get_processes() {
            let pid = Pid::from_raw(*pid_i32);
            trace!(
                "pid: {:?}, name: {:?}, pgid: {:?}, sid: {:?}",
                pid,
                process.name(),
                getpgid(Some(pid)),
                getsid(Some(pid))
            );
        }

        nix::sys::signal::killpg(sid, nix::sys::signal::Signal::SIGKILL)
            .expect("Failed to kill process group");
    }
}

pub fn kill_all() {
    debug!("Killing all farcasterd processes");
    let sys = System::new_all();
    // kill all process matching the following names
    for bin in [
        "farcasterd",
        "swapd",
        "peerd",
        "walletd",
        "grpcd",
        "databased",
        "syncerd",
    ] {
        for proc in sys.get_process_by_name(bin) {
            trace!("Killing process {:?}", proc);
            proc.kill(sysinfo::Signal::Kill);
        }
    }
    debug!("Signal sent for all farcasterd processes...");
}

pub fn cli<T: DeserializeOwned>(
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> Result<T, String> {
    match run_cli(args) {
        Ok((stdout, stderr)) => {
            let s = stdout.join("\n");
            if let Ok(res) = serde_yaml::from_str(&s) {
                Ok(res)
            } else {
                Err(stderr.join(" "))
            }
        }
        Err(e) => Err(e.to_string()),
    }
}

pub fn run_cli(
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> io::Result<(Vec<String>, Vec<String>)> {
    run("../swap-cli", args)
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

pub fn bitcoin_setup() -> bitcoincore_rpc::Client {
    use bitcoincore_rpc::{json::LoadWalletResult, Client, RpcApi};

    let conf = config::TestConfig::parse();
    let bitcoin_rpc =
        Client::new(&format!("{}", conf.bitcoin.daemon), conf.bitcoin.get_auth()).unwrap();

    // make sure a wallet is created and loaded
    // error may be returned because wallet exists, so we try to load the wallet
    if bitcoin_rpc
        .call::<LoadWalletResult>(
            "createwallet",
            &[
                serde_json::to_value("wallet").unwrap(), // wallet_name
                false.into(),                            // disable_private_keys
                false.into(),                            // blank
                serde_json::to_value("").unwrap(),       // passphrase
                false.into(),                            // avoid_reuse
                false.into(),                            // descriptor
            ],
        )
        .is_err()
    {
        // cannot expect on this error as the wallet is sometimes already loaded from previous
        // tests; so even if loading fails, just ignore this error;
        // also, the test fails later if a wallet  problem occurs
        let _ = bitcoin_rpc.load_wallet("wallet");
    }

    sleep(Duration::from_secs(10));

    let address = bitcoin_rpc.get_new_address(None, None).unwrap();
    bitcoin_rpc.generate_to_address(200, &address).unwrap();

    // We mined 200 blocks, allow things to happen, like the electrum server catching up
    sleep(Duration::from_secs(10));

    bitcoin_rpc
}

pub async fn monero_setup() -> (
    monero_rpc::RegtestDaemonJsonRpcClient,
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

    // error happens when wallet does not exist
    if wallet.open_wallet("test".to_string(), None).await.is_err() {
        wallet
            .create_wallet("test".to_string(), None, "English".to_string())
            .await
            .unwrap();
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

pub async fn monero_new_dest_address(
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

    wallet_lock
        .create_wallet(wallet_name.clone(), None, "English".to_string())
        .await
        .unwrap();
    wallet_lock
        .open_wallet(wallet_name.clone(), None)
        .await
        .unwrap();
    let address = wallet_lock.get_address(0, None).await.unwrap();

    (address.address, wallet_name)
}

pub async fn send_monero(
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
            options,
        )
        .await
        .unwrap();
    drop(wallet_lock);
    hex::decode(transaction.tx_hash.to_string()).unwrap()
}

pub fn reusable_btc_address() -> bitcoin::Address {
    bitcoin::Address::from_str("bcrt1q3rc4sm3w9fr6a46n08znfjt7eu2yhhel6j8rsa").unwrap()
}

pub fn reusable_xmr_address() -> monero::Address {
    monero::Address::from_str("44CpGC77Kn6exUWYCUwfaUYmDeKn7MyRcNPikgeHBCz8M6LXUC3fGCWNMW7UACHyTL6QxzqKxvJbu5o2VESLzCaeNHNUkwv").unwrap()
}
