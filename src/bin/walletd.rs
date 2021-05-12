// LNP Node: node running lightning network protocol and generalized lightning
// channels.
// Written in 2020 by
//     Dr. Maxim Orlovsky <orlovsky@pandoracore.com>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the MIT License
// along with this software.
// If not, see <https://opensource.org/licenses/MIT>.

#![recursion_limit = "256"]
// Coding conventions
#![deny(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    unused_mut,
    unused_imports,
    dead_code,
    missing_docs
)]

//! Main executable for walletd: farcaster node wallet  microservice.

#[macro_use]
extern crate log;

use clap::Clap;

use farcaster_node::walletd::{self, Opts};
use farcaster_node::{Config, LogStyle};

use bitcoin::hashes::hex::{FromHex};

fn main() {
    let mut opts = Opts::parse();
    trace!("Command-line arguments: {:?}", &opts);
    opts.process();
    trace!("Processed arguments: {:?}", &opts);

    let config: Config = opts.shared.clone().into();
    trace!("Daemon configuration: {:?}", &config);
    debug!("MSG RPC socket {}", &config.msg_endpoint);
    debug!("CTL RPC socket {}", &config.ctl_endpoint);

    let walletd_token = opts.walletd_token.walletd_token;
    info!("\n\n{}\n\n", walletd_token);
    let token_bytes = Vec::from_hex(&walletd_token);
    info!("\n\n{:?}\n\n", token_bytes);

    let node_id = opts.key_opts.local_node().node_id();
    info!("{}: {}", "Local node id".ended(), node_id.amount());

    debug!("Starting runtime ...");
    walletd::run(config, token_bytes, opts.key_opts.node_secrets()).expect("Error running syncerd runtime");

    unreachable!()
}
