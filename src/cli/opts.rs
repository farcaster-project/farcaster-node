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

use bitcoin::Address as BtcAddress;
use clap_complete::shells::Shell;
use monero::Address as XmrAddress;
use std::net::IpAddr;
use std::str::FromStr;

use farcaster_core::{
    bitcoin::{fee::SatPerVByte, timelock::CSVTimelock},
    blockchain::{Blockchain, FeeStrategy, Network},
    role::SwapRole,
    swap::{btcxmr::PublicOffer, SwapId},
};

/// Command-line tool for working with Farcaster node
#[derive(Parser, Clone, PartialEq, Eq, Debug)]
#[clap(name = "swap-cli", bin_name = "swap-cli", author, version)]
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
        self.shared.process();
    }
}

/// Command-line commands:
#[derive(Subcommand, Clone, PartialEq, Eq, Debug, Display)]
pub enum Command {
    /// General information about the running node
    #[display("info<{subject:?}>")]
    Info {
        /// Remote peer address, swap id, or blockchain and network. If absent, returns information
        /// about the node itself
        subject: Vec<String>,
    },

    /// Lists existing peer connections
    Peers,

    /// Lists running swaps
    #[clap(aliases = &["ls"])]
    ListSwaps,

    /// Lists public offers created by daemon
    #[clap(aliases = &["lo"])]
    ListOffers {
        #[clap(
            short,
            long,
            default_value = "open",
            possible_values = &["open", "Open", "inprogress", "in_progress", "ended", "Ended", "all", "All"],
        )]
        select: OfferSelector,
    },

    /// Gives information on an open offer
    #[clap(aliases = &["oi"])]
    #[display("offer-info<{public_offer}>")]
    OfferInfo {
        /// The offer to be canceled.
        public_offer: PublicOffer,
    },

    /// Lists listeners created by daemon
    #[clap(aliases = &["ll"])]
    ListListens,

    /// Lists tasks currently treated by a syncer
    #[clap(aliases = &["lt"])]
    ListTasks {
        /// The blockchain for which we want to list the tasks
        blockchain: Blockchain,

        /// The network for which we want to list the tasks
        network: Network,
    },

    /// Lists saved checkpoints of the swaps
    #[clap(aliases = &["lc"])]
    ListCheckpoints,

    /// Restores saved checkpoint of a swap
    #[clap(aliases = &["r"])]
    RestoreCheckpoint {
        // The swap id of the swap to be restored.
        swap_id: SwapId,
    },

    /// Maker creates offer and start listening for incoming connections. Command used to to print
    /// the resulting public offer that shall be shared with Taker. Additionally it spins up the
    /// listener awaiting for connection related to this offer.
    ///
    /// Example usage:
    ///
    /// make --btc-addr tb1q4gj53tuew3e6u4a32kdtle2q72su8te39dpceq --xmr-addr
    /// 55LTR8KniP4LQGJSPtbYDacR7dz8RBFnsfAKMaMuwUNYX6aQbBcovzDPyrQF9KXF9tVU6Xk3K8no1BywnJX6GvZX8yJsXvt
    /// --btc-amount "0.0000135 BTC" --xmr-amount "0.001 XMR"
    Make {
        /// Bitcoin address used as destination or refund address.
        #[clap(long = "btc-addr")]
        arbitrating_addr: BtcAddress,

        /// Monero address used as destination or refund address.
        #[clap(long = "xmr-addr")]
        accordant_addr: XmrAddress,

        /// Network to use to execute the swap between the chosen blockchains.
        #[clap(
            short,
            long,
            default_value = "testnet",
            possible_values = &["Testnet", "testnet", "Mainnet", "mainnet", "Local", "local"]
        )]
        network: Network,

        /// The chosen arbitrating blockchain.
        #[clap(
            long = "arb-blockchain",
            default_value = "bitcoin",
            possible_values = &["Bitcoin", "bitcoin"])
        ]
        arbitrating_blockchain: Blockchain,

        /// The chosen accordant blockchain.
        #[clap(
            long = "acc-blockchain",
            default_value = "monero",
            possible_values = &["Monero", "monero"])
        ]
        accordant_blockchain: Blockchain,

        /// Amount of arbitrating assets to exchanged.
        #[clap(long = "btc-amount")]
        arbitrating_amount: bitcoin::Amount,

        /// Amount of accordant assets to exchanged.
        #[clap(long = "xmr-amount")]
        accordant_amount: monero::Amount,

        /// The future maker swap role, either Alice of Bob. This will dictate with asset will be
        /// exchanged for which asset. Alice will sell accordant assets for arbitrating ones and
        /// Bob the inverse, sell arbitrating assets for accordant ones.
        #[clap(short = 'r', long, default_value = "Bob", possible_values = &["Alice", "Bob"])]
        maker_role: SwapRole,

        /// The cancel timelock parameter of the arbitrating blockchain.
        #[clap(long, default_value = "4")]
        cancel_timelock: CSVTimelock,

        /// The punish timelock parameter of the arbitrating blockchain.
        #[clap(long, default_value = "5")]
        punish_timelock: CSVTimelock,

        /// The chosen fee strategy for the arbitrating transactions.
        #[clap(long, default_value = "1 satoshi/vByte")]
        fee_strategy: FeeStrategy<SatPerVByte>,

        /// Public IPv4 or IPv6 address present in the public offer allowing taker to connect.
        #[clap(short = 'I', long, default_value = "127.0.0.1")]
        public_ip_addr: IpAddr,

        /// IPv4 or IPv6 address to bind to, listening for takers.
        #[clap(short, long, default_value = "0.0.0.0")]
        bind_ip_addr: IpAddr,

        /// Port to use; defaults to the native LN port.
        #[clap(short, long, default_value = "9735")]
        port: u16,
    },

    /// Taker accepts offer and connects to maker's daemon to start the trade.
    Take {
        /// Bitcoin address used as destination or refund address.
        #[clap(long = "btc-addr")]
        bitcoin_address: BtcAddress,

        /// Monero address used as destination or refund address.
        #[clap(long = "xmr-addr")]
        monero_address: XmrAddress,

        /// An encoded public offer.
        #[clap(short = 'o', long = "offer")]
        public_offer: PublicOffer,

        /// Accept the public offer without validation.
        #[clap(short, long)]
        without_validation: bool,
    },

    /// Revoke offer accepts an offer and revokes it within the runtime.
    #[display("revoke-offer<{public_offer}>")]
    RevokeOffer {
        /// The offer to be canceled.
        public_offer: PublicOffer,
    },

    /// Abort a swap if it has not locked yet.
    #[display("abort-swap<{swap_id}>")]
    AbortSwap {
        /// The swap to be aborted
        swap_id: SwapId,
    },

    /// Request swap progress report.
    #[display("progress<{swapid}>")]
    Progress {
        /// The swap id requested.
        swapid: SwapId,

        /// Subscribe to progress and only return when progress is finished.
        #[clap(short, long)]
        follow: bool,
    },

    /// Returns addresses and amounts that require funding for blockchain.
    #[display("needs-funding<{blockchain}>")]
    NeedsFunding {
        /// The blockchain funding required needs to be checked against.
        blockchain: Blockchain,
    },

    /// Attempts to sweep any funds on a given bitcoin funding address
    #[display("sweep-bitcoin-address<{source_address} {destination_address}>")]
    SweepBitcoinAddress {
        /// The source address to be swept.
        source_address: BtcAddress,
        /// The destination address receiving the coins.
        destination_address: BtcAddress,
    },

    /// Attempts to sweep any funds on a given monero funding address
    #[display("sweep-monero-address<{source_address} {destination_address}>")]
    SweepMoneroAddress {
        /// The source address to be swept.
        source_address: XmrAddress,
        /// The destination address receiving the coins.
        destination_address: XmrAddress,
    },

    /// Output shell completion code for the specified shell (bash, zsh or fish)
    ///
    /// The shell code must be evaluated to provide interactive completion of swap-cli commands.
    /// This can be done by sourcing it from the .bash_profile.
    ///
    /// A list of usual folders and filenames on Linux
    ///
    /// fish -> /usr/share/fish/vendor_completions.d/farcaster
    ///
    /// bash -> /usr/share/bash-completion/completions/farcaster
    ///
    /// zsh -> /usr/share/zsh/site-functions/_farcaster
    Completion {
        #[clap(value_parser = clap::builder::EnumValueParser::<Shell>::new())]
        shell: Shell,
    },
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Display, From)]
pub enum OfferSelector {
    #[display("Open")]
    Open,
    #[display("In Progress")]
    InProgress,
    #[display("Ended")]
    Ended,
    #[display("All")]
    All,
}

impl FromStr for OfferSelector {
    type Err = OfferSelectorParseError;
    fn from_str(input: &str) -> Result<OfferSelector, Self::Err> {
        match input {
            "open" | "Open" => Ok(OfferSelector::Open),
            "in_progress" | "inprogress" => Ok(OfferSelector::InProgress),
            "ended" | "Ended" => Ok(OfferSelector::Ended),
            "all" | "All" => Ok(OfferSelector::All),
            _ => Err(OfferSelectorParseError::Invalid),
        }
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Display, Error, From)]
#[display(doc_comments)]
pub enum OfferSelectorParseError {
    /// The provided value can't be parsed as an offer selector
    Invalid,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Display, Error, From)]
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
                return Err(AmountOfAssetParseError::NeedsValuePair);
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
                return Err(AmountOfAssetParseError::NeedsValuePair);
            }
        } else {
            return Err(AmountOfAssetParseError::NeedsValuePair);
        }

        let amount = u64::from_str(amount)?;
        let asset = asset.to_owned();

        Ok(AmountOfAsset { asset, amount })
    }
}
