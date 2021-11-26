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

use clap::{AppSettings, Clap};
use farcaster_core::blockchain::Network;
use std::str::FromStr;
use strict_encoding::{StrictDecode, StrictEncode};

/// Syncer blockchain management daemon; part of Farcaster Node
///
/// The daemon is controlled though ZMQ ctl socket (see `ctl-socket` argument
/// description)
#[derive(Clap, Clone, PartialEq, Eq, Debug)]
#[clap(
    name = "syncerd",
    bin_name = "syncerd",
    author,
    version,
    setting = AppSettings::ColoredHelp
)]
pub struct Opts {
    /// These params can be read also from the configuration file, not just
    /// command-line args or environment variables
    #[clap(flatten)]
    pub shared: crate::opts::Opts,

    /// Which coin this syncer should target
    #[clap(long, parse(try_from_str = Coin::from_str))]
    pub coin: Coin,

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
    pub electrum_server: String,

    /// Monero daemon to use for Monero syncers
    #[clap(long)]
    pub monero_daemon: String,

    /// Monero rpc wallet to use for Monero syncers
    #[clap(long)]
    pub monero_rpc_wallet: String,
}

#[derive(Clap, Display, Clone, Hash, PartialEq, Eq, Debug, StrictEncode, StrictDecode)]
#[display(Debug)]
pub enum Coin {
    /// Launches a bitcoin syncer
    Bitcoin,
    /// Launches a monero syncer
    Monero,
}

#[derive(Error, Debug, Display)]
#[display("invalid coin")]
pub enum SyncerCoinError {
    InvalidCoin,
}

impl FromStr for Coin {
    type Err = SyncerCoinError;
    fn from_str(input: &str) -> Result<Coin, Self::Err> {
        match input {
            "Bitcoin" | "bitcoin" => Ok(Coin::Bitcoin),
            "Monero" | "monero" => Ok(Coin::Monero),
            _ => Err(SyncerCoinError::InvalidCoin),
        }
    }
}

impl Opts {
    pub fn process(&mut self) {
        self.shared.process();
    }
}
