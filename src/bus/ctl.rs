use std::fmt::{self, Debug};
use std::str::FromStr;

use farcaster_core::{
    blockchain::Blockchain,
    role::TradeRole,
    swap::btcxmr::{Offer, Parameters, PublicOffer},
    swap::SwapId,
};

use bitcoin::secp256k1::SecretKey;
use bitcoin::Transaction;
use internet2::addr::{InetSocketAddr, NodeAddr};
use strict_encoding::{NetworkDecode, NetworkEncode};

use crate::bus::p2p::{Commit, TakeCommit};
use crate::bus::{
    AddressSecretKey, CheckpointEntry, Failure, OfferStatusPair, OptionDetails, Outcome, Progress,
};
use crate::swapd::CheckpointSwapd;
use crate::syncerd::SweepAddressAddendum;
use crate::walletd::runtime::CheckpointWallet;
use crate::{Error, ServiceId};

#[derive(Clone, Debug, Display, From, NetworkEncode, NetworkDecode)]
#[non_exhaustive]
pub enum CtlMsg {
    #[display("hello()")]
    Hello,

    #[display("terminate()")]
    Terminate,

    #[display(inner)]
    Success(OptionDetails),

    #[display(inner)]
    #[from]
    Failure(Failure),

    #[display(inner)]
    Progress(Progress),

    /// A message sent from farcaster to maker swap service to begin the swap.
    #[display("make_swap({0})")]
    MakeSwap(InitSwap),

    /// A message sent from farcaster to taker swap service to begin the swap.
    #[display("take_swap({0})")]
    TakeSwap(InitSwap),

    /// A message sent from the wallet to notify farcaster the successful initialisation of the
    /// maker/taker wallet.
    #[display("launch_swap({0})")]
    LaunchSwap(LaunchSwap),

    #[display("params({0})")]
    Params(Params),

    #[display("peerd_unreachable({0})")]
    PeerdUnreachable(ServiceId),

    #[display("peerd_reconnected({0})")]
    PeerdReconnected(ServiceId),

    #[display("peerd_terminated()")]
    PeerdTerminated,

    #[display("restore_checkpoint({0})", alt = "{0:#}")]
    RestoreCheckpoint(CheckpointEntry),

    /// A message sent from a client to farcaster to register a new offer a boostrap all necessary
    /// services on farcaster side.
    #[display("make_offer({0})")]
    MakeOffer(ProtoPublicOffer),

    /// A message sent from farcaster to wallet to trigger the creation of the taker wallet.
    #[display("take_offer({0}))")]
    TakeOffer(PubOffer),

    /// A message sent from farcaster to wallet to trigger the creation of the maker wallet after a
    /// taker commit message is received.
    #[display("taker_commited({0}))")]
    TakerCommitted(TakerCommitted),

    #[display("get_keys({0})")]
    GetKeys(GetKeys),

    #[display("revoke_offer({0})")]
    RevokeOffer(PublicOffer),

    #[display("abort_swap()")]
    AbortSwap,

    #[display("get_sweep_bitcoin_address({0})")]
    GetSweepBitcoinAddress(bitcoin::Address),

    #[display("task({0})", alt = "{0:#}")]
    #[from]
    SweepAddress(SweepAddressAddendum),

    #[display("set_address_secret_key")]
    SetAddressSecretKey(AddressSecretKey),

    #[display("funding_updated()")]
    FundingUpdated,

    /// Communicates the result of a swap to services like farcasterd and walletd
    #[display("swap_outcome({0})")]
    SwapOutcome(Outcome),

    #[display("checkpoint({0})", alt = "{0:#}")]
    #[from]
    Checkpoint(Checkpoint),

    #[display("remove_checkpoint")]
    RemoveCheckpoint(SwapId),

    #[display("set_offer_history({0})")]
    SetOfferStatus(OfferStatusPair),

    #[display("keys({0})")]
    Keys(Keys),

    #[display("funding_info({0})")]
    #[from]
    FundingInfo(FundingInfo),

    #[display("bitcoin_address({0})")]
    BitcoinAddress(BitcoinAddress),

    #[display("monero_address({0})")]
    MoneroAddress(MoneroAddress),

    #[display("funding_completed({0})")]
    FundingCompleted(Blockchain),

    #[display("funding_canceled({0})")]
    FundingCanceled(Blockchain),

    #[display("transaction({0})")]
    Tx(Tx),
}

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[display(inner)]
pub enum ProgressStack {
    Progress(Progress),
    Success(OptionDetails),
    Failure(Failure),
}

#[derive(Clone, PartialEq, Eq, Debug, Display, NetworkEncode, NetworkDecode)]
#[display("..")]
pub struct ProtoPublicOffer {
    pub offer: Offer,
    pub public_addr: InetSocketAddr,
    pub bind_addr: InetSocketAddr,
    pub arbitrating_addr: bitcoin::Address,
    pub accordant_addr: monero::Address,
}

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[display("{public_offer}, ..")]
pub struct PubOffer {
    pub public_offer: PublicOffer,
    pub external_address: bitcoin::Address,
    pub internal_address: monero::Address,
}

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[display("{0}, ..")]
pub struct ReconnectPeer(pub NodeAddr, pub Option<SecretKey>);

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode, PartialEq, Eq)]
#[display("{0}")]
pub struct Token(pub String);

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[display("token({0})")]
pub struct GetKeys(pub Token);

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[display("{public_offer}, ..")]
pub struct LaunchSwap {
    pub local_trade_role: TradeRole,
    pub public_offer: PublicOffer,
    pub local_params: Params,
    pub swap_id: SwapId,
    pub remote_commit: Option<Commit>,
    pub funding_address: Option<bitcoin::Address>,
}

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
pub enum Params {
    #[display("alice(..)")]
    Alice(Parameters),
    #[display("bob(..)")]
    Bob(Parameters),
}

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[display("{peerd}, {swap_id}, ..")]
pub struct InitSwap {
    pub peerd: ServiceId,
    pub report_to: Option<ServiceId>,
    pub local_params: Params,
    pub swap_id: SwapId,
    pub remote_commit: Option<Commit>,
    pub funding_address: Option<bitcoin::Address>,
}

#[derive(Clone, Debug, Display, NetworkDecode, NetworkEncode)]
#[display(Debug)]
pub struct Checkpoint {
    pub swap_id: SwapId,
    pub state: CheckpointState,
}

#[derive(Clone, Debug, Display, NetworkDecode, NetworkEncode)]
pub enum CheckpointState {
    #[display("Checkpoint Wallet")]
    CheckpointWallet(CheckpointWallet),
    #[display("Checkpoint Swap")]
    CheckpointSwapd(CheckpointSwapd),
}

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode, Eq, PartialEq)]
#[display(format_keys)]
pub struct Keys(
    pub bitcoin::secp256k1::SecretKey,
    pub bitcoin::secp256k1::PublicKey,
);

fn format_keys(keys: &Keys) -> String {
    format!("sk: {}, pk: {}", keys.0.display_secret(), keys.1,)
}

#[derive(Clone, Debug, Display, NetworkDecode, NetworkEncode)]
pub enum FundingInfo {
    #[display("bitcoin(..)")]
    Bitcoin(BitcoinFundingInfo),
    #[display("monero(..)")]
    Monero(MoneroFundingInfo),
}

#[derive(Clone, Debug, NetworkDecode, NetworkEncode)]
pub struct BitcoinFundingInfo {
    pub swap_id: SwapId,
    pub address: bitcoin::Address,
    pub amount: bitcoin::Amount,
}

impl FromStr for BitcoinFundingInfo {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        let content: Vec<&str> = s.split(' ').collect();

        Ok(BitcoinFundingInfo {
            swap_id: SwapId::from_str(content[0])?,
            amount: bitcoin::Amount::from_str(&format!("{} {}", content[2], content[3]))?,
            address: bitcoin::Address::from_str(content[5])?,
        })
    }
}

impl fmt::Display for BitcoinFundingInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:#?} needs {} to {}",
            self.swap_id, self.amount, self.address
        )
    }
}

#[derive(Clone, Debug, NetworkEncode, NetworkDecode)]
pub struct MoneroFundingInfo {
    pub swap_id: SwapId,
    pub amount: monero::Amount,
    pub address: monero::Address,
}

impl FromStr for MoneroFundingInfo {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        let content: Vec<&str> = s.split(' ').collect();
        Ok(MoneroFundingInfo {
            swap_id: SwapId::from_str(content[0])?,
            amount: monero::Amount::from_str_with_denomination(&format!(
                "{} {}",
                content[2], content[3]
            ))?,

            address: monero::Address::from_str(content[5])?,
        })
    }
}

impl fmt::Display for MoneroFundingInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:#?} needs {} to {}",
            self.swap_id, self.amount, self.address
        )
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Display, NetworkEncode, NetworkDecode)]
#[display("{1}")]
pub struct BitcoinAddress(pub SwapId, pub bitcoin::Address);

#[derive(Clone, PartialEq, Eq, Debug, Display, NetworkEncode, NetworkDecode)]
#[display("{1}")]
pub struct MoneroAddress(pub SwapId, pub monero::Address);

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[display(inner)]
pub enum Tx {
    #[display("lock(..)")]
    Lock(Transaction),
    #[display("buy(..)")]
    Buy(Transaction),
    #[display("funding(..)")]
    Funding(Transaction),
    #[display("cancel(..)")]
    Cancel(Transaction),
    #[display("refund(..)")]
    Refund(Transaction),
    #[display("punish(..)")]
    Punish(Transaction),
}

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[display("taker_commited")]
pub struct TakerCommitted {
    pub swap_id: SwapId,
    pub arbitrating_addr: bitcoin::Address,
    pub accordant_addr: monero::Address,
    pub taker_commit: TakeCommit,
}
