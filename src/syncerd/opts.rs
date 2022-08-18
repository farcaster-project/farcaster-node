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

use farcaster_core::blockchain::{Blockchain, Network};
use std::str::FromStr;

/// Syncer blockchain management daemon; part of Farcaster Node
///
/// The daemon is controlled through ZMQ ctl socket (see `ctl-socket` argument
/// description)
#[derive(Parser, Clone, PartialEq, Eq, Debug)]
#[clap(name = "syncerd", bin_name = "syncerd", author, version)]
pub struct Opts {
    /// These params can be read also from the configuration file, not just
    /// command-line args or environment variables
    #[clap(flatten)]
    pub shared: crate::opts::Opts,

    /// Which blockchain this syncer should target
    #[clap(long, parse(try_from_str = Blockchain::from_str))]
    pub blockchain: Blockchain,

    /// Blockchain networks to use (Mainnet, Testnet, Local)
    #[clap(
        short,
        long,
        global = true,
        alias = "chain",
        default_value = "Testnet",
        parse(try_from_str = Network::from_str)
    )]
    pub network: Network,

    /// Electrum server to use for Bitcoin syncers
    #[clap(long)]
    pub electrum_server: Option<String>,

    /// Monero daemon to use for Monero syncers
    #[clap(long)]
    pub monero_daemon: Option<String>,

    /// Monero rpc wallet to use for Monero syncers
    #[clap(long)]
    pub monero_rpc_wallet: Option<String>,

    /// Monero lws to use for Monero syncers
    #[clap(long)]
    pub monero_lws: Option<String>,

    /// Wallet directory use by the monero-wallet-rpc
    #[clap(long)]
    pub monero_wallet_dir_path: Option<String>,
}

impl Opts {
    pub fn process(&mut self) {
        self.shared.process();
    }
}
