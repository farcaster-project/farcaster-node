// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use std::fmt::{self, Debug};
use std::io;
use std::str::FromStr;

use farcaster_core::blockchain::Network;
use farcaster_core::consensus::{self, Decodable, Encodable};
use farcaster_core::impl_strict_encoding;
use farcaster_core::swap::btcxmr::KeyManager;
use farcaster_core::{
    blockchain::Blockchain,
    swap::btcxmr::{Deal, DealParameters, Parameters},
    swap::SwapId,
};

use bitcoin::secp256k1::SecretKey;
use bitcoin::Transaction;
use internet2::addr::{InetSocketAddr, NodeAddr};
use strict_encoding::{NetworkDecode, NetworkEncode};

use crate::bus::p2p::{PeerMsg, TakerCommit};
use crate::bus::{
    AddressSecretKey, CheckpointEntry, DealStatusPair, Failure, OptionDetails, Outcome, Progress,
};
use crate::swapd::CheckpointSwapd;
use crate::syncerd::{Health, SweepAddressAddendum};
use crate::{Error, ServiceId};

use super::p2p::Commit;

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

    /// A message sent from farcaster to database on startup to cleanup dangling deal data
    CleanDanglingDeals,

    /// A message sent from farcaster to maker swap service to begin the swap.
    #[display("make_swap({0})")]
    MakeSwap(InitMakerSwap),

    /// A message sent from farcaster to taker swap service to begin the swap.
    #[display("take_swap({0})")]
    TakeSwap(InitTakerSwap),

    /// A message sent from farcaster to wallet service to create swap keys.
    #[display("create_swap_keys({0})")]
    CreateSwapKeys(Deal, Token),

    // A message sent from wallet to farcaster containing keys for a swap.
    #[display("swap_keys({0})")]
    SwapKeys(SwapKeys),

    #[display("params({0})")]
    Params(Params),

    #[display("peerd_unreachable({0})")]
    PeerdUnreachable(ServiceId),

    #[display("peerd_reconnected({0})")]
    PeerdReconnected(ServiceId),

    #[display("peerd_terminated()")]
    PeerdTerminated,

    #[display("disconnected")]
    Disconnected,

    #[display("re-connected")]
    Reconnected,

    #[display("connect({0})")]
    Connect(SwapId),

    #[display("Connect success")]
    ConnectSuccess,

    #[display("restore_checkpoint({0})", alt = "{0:#}")]
    RestoreCheckpoint(CheckpointEntry),

    /// A message sent from a client to farcaster to register a new deal a boostrap all necessary
    /// services on farcaster side.
    #[display("make_deal({0})")]
    MakeDeal(ProtoDeal),

    /// A message sent from farcaster to wallet to trigger the creation of the taker wallet.
    #[display("take_deal({0}))")]
    TakeDeal(PubDeal),

    /// A message sent from farcaster to wallet to trigger the creation of the maker wallet after a
    /// taker commit message is received.
    #[display("taker_commited({0}))")]
    TakerCommitted(TakerCommitted),

    #[display("get_keys({0})")]
    GetKeys(GetKeys),

    #[display("revoke_deal({0})")]
    RevokeDeal(Deal),

    #[display("abort_swap()")]
    AbortSwap,

    #[display("get_sweep_bitcoin_address({0})")]
    GetSweepBitcoinAddress(bitcoin::Address),

    #[display("task({0})", alt = "{0:#}")]
    #[from]
    SweepAddress(SweepAddressAddendum),

    #[display("set_address_secret_key")]
    SetAddressSecretKey(AddressSecretKey),

    #[display("get_balance")]
    GetBalance(AddressSecretKey),

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

    #[display("set_deal_history({0})")]
    SetDealStatus(DealStatusPair),

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

    #[display("failed peer message")]
    FailedPeerMessage(PeerMsg),

    #[display("connect failed")]
    ConnectFailed,

    #[display("health_check({0} {1})")]
    HealthCheck(Blockchain, Network),

    #[display("health_result({0})")]
    HealthResult(Health),
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
pub struct ProtoDeal {
    pub deal_parameters: DealParameters,
    pub public_addr: InetSocketAddr,
    pub arbitrating_addr: bitcoin::Address,
    pub accordant_addr: monero::Address,
}

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[display("{deal}, ..")]
pub struct PubDeal {
    pub deal: Deal,
    pub bitcoin_address: bitcoin::Address,
    pub monero_address: monero::Address,
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
#[display("{deal}, ..")]
pub struct SwapKeys {
    pub key_manager: WrappedKeyManager,
    pub deal: Deal,
}

#[derive(Clone, Debug)]
pub struct WrappedKeyManager(pub KeyManager);
impl Encodable for WrappedKeyManager {
    fn consensus_encode<W: io::Write>(&self, writer: &mut W) -> Result<usize, io::Error> {
        Ok(self.0.consensus_encode(writer)?)
    }
}
impl Decodable for WrappedKeyManager {
    fn consensus_decode<D: io::Read>(d: &mut D) -> Result<Self, consensus::Error> {
        Ok(WrappedKeyManager(Decodable::consensus_decode(d)?))
    }
}
impl_strict_encoding!(WrappedKeyManager);

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
pub enum Params {
    #[display("alice(..)")]
    Alice(Parameters),
    #[display("bob(..)")]
    Bob(Parameters),
}

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[display("{swap_id}, ..")]
pub struct InitTakerSwap {
    pub peerd: ServiceId,
    pub report_to: ServiceId,
    pub swap_id: SwapId,
    pub key_manager: WrappedKeyManager,
    pub target_bitcoin_address: bitcoin::Address,
    pub target_monero_address: monero::Address,
}

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[display("{swap_id}, ..")]
pub struct InitMakerSwap {
    pub peerd: ServiceId,
    pub report_to: ServiceId,
    pub swap_id: SwapId,
    pub key_manager: WrappedKeyManager,
    pub target_bitcoin_address: bitcoin::Address,
    pub target_monero_address: monero::Address,
    pub commit: Commit,
}

#[derive(Clone, Debug, Display, NetworkDecode, NetworkEncode)]
#[display(Debug)]
pub struct Checkpoint {
    pub swap_id: SwapId,
    pub state: CheckpointSwapd,
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

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, Debug, Display, Eq, PartialEq, NetworkDecode, NetworkEncode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub enum FundingInfo {
    #[display("{0}")]
    Bitcoin(BitcoinFundingInfo),
    #[display("{0}")]
    Monero(MoneroFundingInfo),
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, Debug, Eq, PartialEq, NetworkDecode, NetworkEncode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub struct BitcoinFundingInfo {
    pub swap_id: SwapId,
    pub address: bitcoin::Address,
    #[serde(with = "bitcoin::util::amount::serde::as_btc")]
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

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, Debug, Eq, PartialEq, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub struct MoneroFundingInfo {
    pub swap_id: SwapId,
    #[serde(with = "monero::util::amount::serde::as_xmr")]
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
    pub taker_commit: TakerCommit,
}
