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
    #[clap(parse(try_from_str = Coin::from_str))]
    pub coin: Coin,
}

#[derive(Clap, Clone, Hash, PartialEq, Eq, Debug, StrictEncode, StrictDecode)]
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
            "bitcoin" => Ok(Coin::Bitcoin),
            "monero" => Ok(Coin::Monero),
            _ => Err(SyncerCoinError::InvalidCoin),
        }
    }
}

impl ToString for Coin {
    fn to_string(&self) -> String {
        match self {
            Coin::Bitcoin => "bitcoin".to_string(),
            Coin::Monero => "monero".to_string(),
        }
    }
}

impl Opts {
    pub fn process(&mut self) {
        self.shared.process();
    }
}
