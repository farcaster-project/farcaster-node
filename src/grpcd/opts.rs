// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

/// Grpcd daemon; part of Farcaster Node
#[derive(Parser, Clone, PartialEq, Eq, Debug)]
#[clap(name = "grpcd", bin_name = "grpcd", author, version)]
pub struct Opts {
    /// These params can be read also from the configuration file, not just
    /// command-line args or environment variables
    #[clap(flatten)]
    pub shared: crate::opts::Opts,

    /// Port number that the grpc server is accepting connections on
    #[clap(long)]
    pub grpc_port: u16,

    /// Ip that the grpc server is accepting connections on
    #[clap(long)]
    pub grpc_ip: String,
}

impl Opts {
    pub fn process(&mut self) {
        self.shared.process();
    }
}
