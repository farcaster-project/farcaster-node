// #![cfg(feature = "integration_test")]

use std::process;
use std::{thread, time};
use sysinfo::{ProcessExt, System, SystemExt};

#[test]
fn spawn_swap() {
    // Config data dir for two fcd
    let data_dir_maker = vec!["-d", "tests/fc1", "-vv"];
    let data_dir_taker = vec!["-d", "tests/fc2", "-vv"];

    // Get path to target/debug bins
    let mut bin_path = std::env::current_exe().unwrap();
    bin_path.pop();

    // Swap-cli bin path
    let mut client_bin_path = bin_path.clone();
    client_bin_path.push("../swap-cli");

    // Farcasterd bin path
    bin_path.push("../farcasterd");

    // Launch fcd node 0
    let mut cmd = process::Command::new(bin_path.clone());
    cmd.args(data_dir_maker.clone());
    let mut farcasterd_maker = cmd.spawn().unwrap();

    // Launch fcd node 1
    // FIXME this node is not used
    let mut cmd = process::Command::new(bin_path);
    cmd.args(data_dir_taker.clone());
    let mut farcasterd_taker = cmd.spawn().unwrap();

    // Wait for fcd to launch
    thread::sleep(time::Duration::from_secs_f32(4.0));

    // set up maker

    // make tb1q4gj53tuew3e6u4a32kdtle2q72su8te39dpceq
    // 55LTR8KniP4LQGJSPtbYDacR7dz8RBFnsfAKMaMuwUNYX6aQbBcovzDPyrQF9KXF9tVU6Xk3K8no1BywnJX6GvZX8yJsXvt
    // Testnet ECDSA Monero "101 BTC" "100 XMR" Alice 10 30 "1 satoshi/vByte" "127.0.0.1" "0.0.0.0" 9376

    let mut cmd = process::Command::new(client_bin_path.clone());
    cmd.args(data_dir_maker.clone());
    cmd.args(vec![
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
            "ECDSA",
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
            ]);

    println!("executing command: {:?}", cmd);
    let _ = cmd.spawn().unwrap();

    // set up taker
    // FIXME this cannot work as the identify of maker is not correct in public offer, this should
    // inject result from past command

    //let mut cmd = process::Command::new(client_bin_path);
    //cmd.args(data_dir_taker.clone());
    //cmd.args(vec![
    //            "take",
    //            "-w",
    //            "tb1q4gj53tuew3e6u4a32kdtle2q72su8te39dpceq",
    //            "55LTR8KniP4LQGJSPtbYDacR7dz8RBFnsfAKMaMuwUNYX6aQbBcovzDPyrQF9KXF9tVU6Xk3K8no1BywnJX6GvZX8yJsXvt",
    //            "Offer:Cke4ftrP5A71LQM2fvVdFMNR4gdNgpBKX11111uMFj3o3qSdopKe2zu11111TBJQ23113GTvtvqfD1111112g5B8XgrFP91Y3iiLra8gGGsbrTcXkbEe6QqWDRirEYtXkCMp11111111111111111111111111111111111111111AfZ113Wigk7Ghe3R",
    //        ]);

    //println!("executing command: {:?}", cmd);
    //let _ = cmd.spawn().unwrap();

    // clean up processes
    thread::sleep(time::Duration::from_secs_f32(5.0));

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
        let ps_out = process::Command::new("ps")
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
