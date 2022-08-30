//! Helper functions to launch, control, and clean farcaster instances.

use std::env;
use std::ffi::OsStr;
use std::io;
use std::process;
use std::str;
use std::thread::sleep;
use std::time::Duration;

use serde_crate::de::DeserializeOwned;
use sysinfo::{ProcessExt, System, SystemExt};

use super::config;

// TODO: rename this function, this launches fcd, not 'clients'
pub async fn setup_clients() -> (process::Child, Vec<String>, process::Child, Vec<String>) {
    // data directories
    let data_dir_maker = vec!["-d".to_string(), "tests/fc1".to_string()];
    let data_dir_taker = vec!["-d".to_string(), "tests/fc2".to_string()];

    // If we are in CI we use .ci.toml files, otherwise .toml
    let ctx = env::var("CI").unwrap_or_else(|_| "false".into());
    let ext = if ctx == "false" { ".toml" } else { ".ci.toml" };

    let farcasterd_maker_args = farcasterd_args(
        data_dir_maker.clone(),
        vec!["--config", &format!("tests/cfg/fc1{}", ext)],
        vec![],
    );
    let farcasterd_taker_args = farcasterd_args(
        data_dir_taker.clone(),
        vec!["--config", &format!("tests/cfg/fc2{}", ext)],
        vec![],
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

fn farcasterd_args(data_dir: Vec<String>, server_args: Vec<&str>, extra: Vec<&str>) -> Vec<String> {
    data_dir
        .into_iter()
        .chain(server_args.into_iter().map(|s| s.into()))
        .chain(extra.into_iter().map(|s| s.into()))
        .collect()
}

fn launch(name: &str, args: impl IntoIterator<Item = String>) -> io::Result<process::Child> {
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

pub fn cleanup_processes(mut farcasterds: Vec<process::Child>) {
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
            ["swapd", "grpcd", "databased", "walletd", "syncerd", "peerd"].contains(&process.name())
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
