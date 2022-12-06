// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

//! Command-line interface to LNP node

#[macro_use]
extern crate log;

use clap::Parser;

use farcaster_node::cli::Opts;
use farcaster_node::client::Client;
use farcaster_node::LogStyle;
use farcaster_node::ServiceConfig;
use microservices::shell::Exec;

fn main() {
    let mut opts = Opts::parse();
    trace!("Command-line arguments: {:?}", &opts);
    opts.process();
    trace!("Processed arguments: {:?}", &opts);

    let service_config: ServiceConfig = opts.shared.clone().into();
    trace!("Daemon configuration: {:#?}", &service_config);
    debug!("MSG RPC socket {}", &service_config.msg_endpoint);
    debug!("CTL RPC socket {}", &service_config.ctl_endpoint);

    let mut client = Client::with(service_config).expect("Error initializing client");

    trace!("Executing command: {:?}", opts.command);

    if let Err(err) = opts.command.exec(&mut client) {
        eprintln!("{} {}", "error:".err(), err.err());
        std::process::exit(1);
    }
}
