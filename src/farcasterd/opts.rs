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

use clap::ValueHint;

pub const FARCASTER_CONFIG: &str = "{data_dir}/farcasterd.toml";

/// Farcaster node management daemon; part of Farcaster Node
///
/// The daemon is controlled through ZMQ ctl socket (see `ctl-socket` argument
/// description)
#[derive(Parser, Clone, PartialEq, Eq, Debug)]
#[clap(name = "farcasterd", bin_name = "farcasterd", author, version)]
pub struct Opts {
    /// These params can be read also from the configuration file, not just
    /// command-line args or environment variables
    #[clap(flatten)]
    pub shared: crate::opts::Opts,

    /// Path to the configuration file.
    #[clap(
        short,
        long,
        global = true,
        env = "FARCASTER_CONFIG",
        default_value = FARCASTER_CONFIG,
        value_hint = ValueHint::FilePath
    )]
    pub config: String,
}

impl Opts {
    pub fn process(&mut self) {
        self.shared.process();
        self.shared.process_dir(&mut self.config);
    }
}
