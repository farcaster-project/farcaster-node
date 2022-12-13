// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

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

//! Main executable for walletd: Farcaster Node wallet microservice.

#[macro_use]
extern crate log;

use clap::Parser;

use farcaster_node::ServiceConfig;
use farcaster_node::{
    bus::ctl::Token,
    walletd::{self, NodeSecrets, Opts},
};

fn main() {
    let mut opts = Opts::parse();
    trace!("Command-line arguments: {:?}", &opts);
    opts.process();
    trace!("Processed arguments: {:?}", &opts);

    let service_config: ServiceConfig = opts.shared.clone().into();
    trace!("Daemon configuration: {:#?}", &service_config);
    debug!("MSG RPC socket {}", &service_config.msg_endpoint);
    debug!("CTL RPC socket {}", &service_config.ctl_endpoint);

    let wallet_token = Token(opts.wallet_token.token);

    let node_secrets = NodeSecrets::new(opts.key_opts.key_file.clone());

    debug!("Starting runtime ...");
    walletd::run(service_config, wallet_token, node_secrets)
        .expect("Error running walletd runtime");

    unreachable!()
}
