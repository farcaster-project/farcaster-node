//! Helper functions to launch, control, and clean farcaster instances.

use std::env;
use std::ffi::OsStr;
use std::io;
use std::ops::Deref;
use std::process;
use std::str;
use std::thread::sleep;
use std::time::Duration;

use serde_crate::de::DeserializeOwned;
use sysinfo::{ProcessExt, System, SystemExt};

use super::config;

pub async fn launch_farcasterd_instances() -> (
    FarcasterdProcess,
    Vec<String>,
    FarcasterdProcess,
    Vec<String>,
) {
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
        FarcasterdProcess(farcasterd_maker),
        data_dir_maker,
        FarcasterdProcess(farcasterd_taker),
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

pub struct FarcasterdProcess(process::Child);

impl Deref for FarcasterdProcess {
    type Target = process::Child;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for FarcasterdProcess {
    fn drop(&mut self) {
        // clean up process
        let sys = System::new_all();
        let farcasterd_process = &mut self.0;

        // there should be no problem in doing the conversion from u32 to i32, since
        // sys.get_processes() returns i32s
        let farcasterd_pid = farcasterd_process.id() as i32;

        // those are peerd processes that are children of the farcasterd process
        let peerd_procs: Vec<_> = sys
            .get_processes()
            .iter()
            .filter_map(|(pid, process)| {
                let process_ppid = process.parent()?;

                let is_peerd = process.name() == "peerd";
                let is_child_of_farcasterd = process_ppid == farcasterd_pid;

                if is_peerd && is_child_of_farcasterd {
                    Some(*pid)
                } else {
                    None
                }
            })
            .collect();

        // a peerd process may have other peerd processes as children, so we kill then;
        let peerds_children_of_peerds = sys.get_processes().iter().filter_map(|(pid, process)| {
            let process_ppid = process.parent()?;

            let is_peerd = process.name() == "peerd";
            let is_child_of_peerd = peerd_procs
                .iter()
                .any(|&main_peerd_pid| main_peerd_pid == process_ppid);

            if is_peerd && is_child_of_peerd {
                Some(*pid)
            } else {
                None
            }
        });
        peerds_children_of_peerds.for_each(|pid| {
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGINT,
            )
            .expect("Sending CTRL-C failed");
        });

        peerd_procs.iter().for_each(|&pid| {
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGINT,
            )
            .expect("Sending CTRL-C failed");
        });

        // kill all other processes spawned by farcasterd
        sys.get_processes()
            .iter()
            .filter_map(|(pid, process)| {
                let process_ppid = process.parent()?;

                let is_daemon_process =
                    ["swapd", "grpcd", "databased", "walletd", "syncerd"].contains(&process.name());
                let is_child_of_farcasterd = process_ppid == farcasterd_pid;

                if is_daemon_process && is_child_of_farcasterd {
                    Some(*pid)
                } else {
                    None
                }
            })
            .for_each(|pid| {
                nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid),
                    nix::sys::signal::Signal::SIGINT,
                )
                .expect("Sending CTRL-C failed");
            });

        farcasterd_process.kill().expect("Couldn't kill farcasterd");
    }
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
