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
use std::net::IpAddr;
use std::path::PathBuf;
use std::str::FromStr;

use internet2::{FramingProtocol, PartialNodeAddr};
use lnp::{ChannelId as SwapId, TempChannelId as TempSwapId};

use farcaster_chains::{
    bitcoin::{Amount, Bitcoin, CSVTimelock},
    monero::Monero,
    pairs::btcxmr::BtcXmr,
};
use farcaster_core::{
    blockchain::{FeeStrategy, Network},
    role::SwapRole,
    negotiation::PublicOffer,
};

/// Command-line tool for working with Farcaster node
#[derive(Clap, Clone, PartialEq, Eq, Debug)]
#[clap(
    name = "swap-cli",
    bin_name = "swap-cli",
    author,
    version,
    setting = AppSettings::ColoredHelp
)]
pub struct Opts {
    /// These params can be read also from the configuration file, not just
    /// command-line args or environment variables
    #[clap(flatten)]
    pub shared: crate::opts::Opts,

    /// Command to execute
    #[clap(subcommand)]
    pub command: Command,
}

impl Opts {
    pub fn process(&mut self) {
        self.shared.process()
    }
}
/// Command-line commands:
#[derive(Clap, Clone, PartialEq, Eq, Debug, Display)]
pub enum Command {
    /// Bind to a socket and start listening for incoming LN peer connections,
    /// Maker's action
    #[display("listen<{overlay}://{ip_addr}:{port}>")]
    Listen {
        /// IPv4 or IPv6 address to bind to
        #[clap(short, long = "ip", default_value = "0.0.0.0")]
        ip_addr: IpAddr,

        /// Port to use; defaults to the native LN port.
        #[clap(short, long, default_value = "9735")]
        port: u16,

        /// Use overlay protocol (http, websocket etc)
        #[clap(short, long, default_value = "tcp")]
        overlay: FramingProtocol,
    },

    /// Connect to the remote lightning network peer
    Connect {
        /// Address of the remote node, in
        /// '<public_key>@<ipv4>|<ipv6>|<onionv2>|<onionv3>[:<port>]' format
        peer: PartialNodeAddr,
    },

    /// Ping remote peer (must be already connected)
    Ping {
        /// Address of the remote node, in
        /// '<public_key>@<ipv4>|<ipv6>|<onionv2>|<onionv3>[:<port>]' format
        peer: PartialNodeAddr,
    },

    /// General information about the running node
    Info {
        /// Remote peer address or temporary/permanent/short channel id. If
        /// absent, returns information about the node itself
        subject: Option<String>,
    },

    /// Lists existing peer connections
    Peers,

    /// Lists running swaps
    Ls,

    /// Maker creates offer and start listening. Command used to to print a hex
    /// representation of the offer that shall be shared with Taker.
    /// Additionally it spins up the listener awaiting for connection related to
    /// this offer. Example usage: `make Testnet Bitcoin Monero 100000 200 10 10
    /// 20 Alice`
    Make {
        /// Type of offer and network to use
        #[clap(default_value = "Testnet")]
        network: Network,

        /// The chosen arbitrating blockchain
        #[clap(default_value = "Bitcoin")]
        arbitrating: Bitcoin,

        /// The chosen accordant blockchain
        #[clap(default_value = "Monero")]
        accordant: Monero,

        /// Amount of arbitrating assets to exchanged
        #[clap(default_value = "100")]
        arbitrating_assets: Amount,

        /// Amount of accordant assets to exchanged
        #[clap(default_value = "100")]
        accordant_assets: u64,

        /// The cancel timelock parameter of the arbitrating blockchain
        #[clap(default_value = "10")]
        cancel_timelock: CSVTimelock,

        /// The punish timelock parameter of the arbitrating blockchain
        #[clap(default_value = "30")]
        punish_timelock: CSVTimelock,

        /// The chosen fee strategy for the arbitrating transactions
        #[clap(default_value = "20")]
        fee_strategy: FeeStrategy<farcaster_chains::bitcoin::fee::SatPerVByte>,

        /// The future maker swap role
        #[clap(default_value = "Alice", possible_values = &["Alice", "Bob"])]
        maker_role: SwapRole,

        /// IPv4 or IPv6 address to bind to
        #[clap(default_value = "0.0.0.0")]
        ip_addr: IpAddr,

        /// Port to use; defaults to the native LN port.
        #[clap(default_value = "9735")]
        port: u16,

        /// Use overlay protocol (http, websocket etc)
        #[clap(default_value = "tcp")]
        overlay: FramingProtocol,
    },
    /// Taker accepts offer and connects to Maker's daemon.
    Take {
        /// Hex encoded offer
        public_offer: PublicOffer<BtcXmr>,
    }
}

#[derive(
    Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Display, Error, From,
)]
#[display(doc_comments)]
pub enum AmountOfAssetParseError {
    /// The provided value can't be parsed as a pair of asset name/ticker and
    /// asset amount; use <asset>:<amount> or '<amount> <asset>' form and do
    /// not forget about quotation marks in the second case
    NeedsValuePair,

    /// The provided amount can't be interpreted; please use unsigned integer
    #[from(std::num::ParseIntError)]
    InvalidAmount,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Display)]
#[display("{amount} {asset}", alt = "{asset}:{amount}")]
pub struct AmountOfAsset {
    /// Asset ticker
    asset: String,

    /// Amount of the asset in atomic units
    amount: u64,
}

impl FromStr for AmountOfAsset {
    type Err = AmountOfAssetParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (asset, amount);
        if s.contains(':') {
            let mut split = s.split(':');
            asset = split
                .next()
                .ok_or(AmountOfAssetParseError::NeedsValuePair)?;
            amount = split
                .next()
                .ok_or(AmountOfAssetParseError::NeedsValuePair)?;
            if split.count() > 0 {
                Err(AmountOfAssetParseError::NeedsValuePair)?
            }
        } else if s.contains(' ') {
            let mut split = s.split(' ');
            amount = split
                .next()
                .ok_or(AmountOfAssetParseError::NeedsValuePair)?;
            asset = split
                .next()
                .ok_or(AmountOfAssetParseError::NeedsValuePair)?;
            if split.count() > 0 {
                Err(AmountOfAssetParseError::NeedsValuePair)?
            }
        } else {
            return Err(AmountOfAssetParseError::NeedsValuePair);
        }

        let amount = u64::from_str(amount)?;
        let asset = asset.to_owned();

        Ok(AmountOfAsset { asset, amount })
    }
}
