// #![cfg(feature = "integration_test")]
#[macro_use]
extern crate log;

use clap::Clap;
use farcaster_node::rpc::Client;
use farcaster_node::{Config, LogStyle};
use sysinfo::{ProcessExt, System, SystemExt};

use farcaster_node::cli::Command;
use microservices::shell::Exec;

#[test]
fn spawn_swap() {
    use farcaster_node::{cli::Opts, farcasterd::launch};
    let data_dir_maker = vec!["-d", "tests/.farcaster_node_0"];
    let data_dir_taker = vec!["-d", "tests/.farcaster_node_1"];
    let mut opts = Opts::parse_from(
        vec!["swap-cli"]
            .into_iter()
            .chain(data_dir_maker.clone())
            .chain(vec!["make", "tb1q4gj53tuew3e6u4a32kdtle2q72su8te39dpceq", "55LTR8KniP4LQGJSPtbYDacR7dz8RBFnsfAKMaMuwUNYX6aQbBcovzDPyrQF9KXF9tVU6Xk3K8no1BywnJX6GvZX8yJsXvt"]),
    );
    opts.process();
    info!("opts: {:?}", opts);
    let config: Config = opts.shared.clone().into();

    let mut client =
        Client::with(config.clone(), config.chain.clone()).expect("Error running client");

    let mut farcasterd_maker = launch("../farcasterd", data_dir_maker.clone()).unwrap();
    let mut farcasterd_taker = launch("../farcasterd", data_dir_taker.clone()).unwrap();

    use std::{thread, time};

    // Command::Ls
    //     .exec(&mut client)
    //     .unwrap_or_else(|err| eprintln!("{} {}", "error:".err(), err.err()));

    info!("executing command: {:?}", opts.command);
    opts.command
        .exec(&mut client)
        .unwrap_or_else(|err| eprintln!("{} {}", "error:".err(), err.err()));

    // set up maker

    // make tb1q4gj53tuew3e6u4a32kdtle2q72su8te39dpceq 55LTR8KniP4LQGJSPtbYDacR7dz8RBFnsfAKMaMuwUNYX6aQbBcovzDPyrQF9KXF9tVU6Xk3K8no1BywnJX6GvZX8yJsXvt Testnet ECDSA Monero
    // "0.00001350 BTC" "0.00000001 XMR" Alice 10 30 "1 satoshi/vByte" "127.0.0.1"
    // "0.0.0.0" 9745

    opts = Opts::parse_from(
        vec!["swap-cli"]
            .into_iter()
            .chain(data_dir_maker.clone())
            .chain(vec![
                "make",
                "tb1q4gj53tuew3e6u4a32kdtle2q72su8te39dpceq",
                "55LTR8KniP4LQGJSPtbYDacR7dz8RBFnsfAKMaMuwUNYX6aQbBcovzDPyrQF9KXF9tVU6Xk3K8no1BywnJX6GvZX8yJsXvt",
                "Testnet",
                "ECDSA",
                "Monero",
                "101 BTC",
                "100 XMR",
                "Alice",
                "10",
                "30",
                "1 satoshi/vByte",
                "127.0.0.1",
                "0.0.0.0",
                "9376",
            ]),
    );
    opts.command
        .exec(&mut client)
        .unwrap_or_else(|err| eprintln!("{} {}", "error:".err(), err.err()));

    // set up taker

    opts = Opts::parse_from(
        vec!["swap-cli"]
            .into_iter()
            .chain(data_dir_taker.clone())
            .chain(vec![
                "take", "tb1q4gj53tuew3e6u4a32kdtle2q72su8te39dpceq", "55LTR8KniP4LQGJSPtbYDacR7dz8RBFnsfAKMaMuwUNYX6aQbBcovzDPyrQF9KXF9tVU6Xk3K8no1BywnJX6GvZX8yJsXvt", "4643535741500100020000008080000080080000c5015a02000000080000407a10f35a000004000a00000004001e000000010800010000000000000001210002c73234cf6408536ddc7f1ad0536b0d1bb002b47c05f992767674676b3a2c68d70000000000000000000000000000000000000000000000000000000000000000000024a000",
            ]),
    );
    opts.command
        .exec(&mut client)
        .unwrap_or_else(|err| eprintln!("{} {}", "error:".err(), err.err()));

    // clean up processes

    thread::sleep(time::Duration::from_secs_f32(2.0));
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

    #[cfg(feature = "integration_test")]
    {
        let ps_out = std::process::Command::new("ps")
            .args(&["-e"])
            .output()
            .expect("failed to execute process")
            .stdout;

        use regex::RegexSet;
        let re = RegexSet::new(&[r" farcasterd", r" peerd", r" swapd", r" walletd"]).unwrap();

        let matches: Vec<_> = re
            .matches(std::str::from_utf8(&ps_out).unwrap())
            .into_iter()
            .collect();
        // fails if there are any lingering `farcasterd`s or `peerd`s, so this test is a
        // false-negative if any are running independent of the test
        assert_eq!(matches, vec![0]);
    }
}
