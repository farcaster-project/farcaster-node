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

use farcaster_core::{
    role::TradeRole,
    swap::{btcxmr::PublicOffer, SwapId},
};
use std::str::FromStr;

/// Swap executor daemon; part of Farcaster Node
///
/// The daemon is controlled through ZMQ ctl socket (see `ctl-socket` argument
/// description)
#[derive(Parser, Clone, PartialEq, Eq, Debug)]
#[clap(name = "swapd", bin_name = "swapd", author, version)]
pub struct Opts {
    /// The unique swap identifier; used between nodes to reference this unique swap
    #[clap(long = "id", parse(try_from_str = SwapId::from_str))]
    pub swap_id: SwapId,

    /// Public offer to initiate the swap runtime
    #[clap(long, parse(try_from_str = FromStr::from_str))]
    pub public_offer: PublicOffer,

    /// Trade role to execute; maker propose the trade, taker accept the trade. Swap role (Alice or
    /// Bob) is defined by the executed trade role and the public offer, if we are the maker we
    /// execute the `maker role` defined in the public offer, otherwise we execute the
    /// complementary role
    #[clap(long, parse(try_from_str = FromStr::from_str), possible_values = &["maker", "Maker", "taker", "Taker"])]
    pub trade_role: TradeRole,

    /// Finality argument used for the arbitrating blockchain; defines when transactions are
    /// considered final
    #[clap(long = "arb-finality")]
    pub arbitrating_finality: u8,

    /// Safety argument used for the arbitrating blockchain to avoid races; a transaction will not
    /// be broadcasted if the safety number of blocks unmined before unlocking another transaction
    /// is not respected
    #[clap(long = "arb-safety")]
    pub arbitrating_safety: u8,

    /// Finality argument used for the accordant blockchain; defines when transactions are
    /// considered final
    #[clap(long = "acc-finality")]
    pub accordant_finality: u8,

    /// These params can be read also from the configuration file, not just
    /// Command-line args or environment variables
    #[clap(flatten)]
    pub shared: crate::opts::Opts,
}

impl Opts {
    pub fn process(&mut self) {
        self.shared.process();
    }
}
