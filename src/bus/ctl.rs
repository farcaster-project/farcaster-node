use std::fmt::{self, Debug, Display, Formatter};
use std::str::FromStr;

use farcaster_core::{
    blockchain::Blockchain,
    role::TradeRole,
    swap::btcxmr::{Offer, Parameters, PublicOffer},
    swap::SwapId,
};

use amplify::{ToYamlString, Wrapper};
use bitcoin::secp256k1::SecretKey;
use bitcoin::Transaction;
use internet2::addr::{InetSocketAddr, NodeAddr};
use microservices::rpc;
use strict_encoding::{NetworkDecode, NetworkEncode};

use crate::bus::msg::Commit;
use crate::bus::rpc::{AddressSecretKey, OfferStatus};
use crate::bus::BusMsg;
use crate::swapd::CheckpointSwapd;
use crate::syncerd::SweepAddressAddendum;
use crate::walletd::runtime::CheckpointWallet;
use crate::{Error, ServiceId};

#[derive(Clone, Debug, Display, From, NetworkEncode, NetworkDecode)]
#[non_exhaustive]
pub enum Ctl {
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

    #[display("make_swap({0})")]
    MakeSwap(InitSwap),

    #[display("take_swap({0})")]
    TakeSwap(InitSwap),

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
    RestoreCheckpoint(SwapId),

    #[display("make_offer({0})")]
    MakeOffer(ProtoPublicOffer),

    #[display("take_offer({0}))")]
    TakeOffer(PubOffer),

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

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[display(inner)]
pub enum Progress {
    Message(String),
    StateTransition(String),
}

#[derive(Wrapper, Clone, PartialEq, Eq, Debug, From, Default, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub struct OptionDetails(pub Option<String>);

impl Display for OptionDetails {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.as_inner() {
            None => Ok(()),
            Some(msg) => f.write_str(msg),
        }
    }
}

impl OptionDetails {
    pub fn with(s: impl ToString) -> Self {
        Self(Some(s.to_string()))
    }

    pub fn new() -> Self {
        Self(None)
    }
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

#[derive(Clone, Debug, Eq, PartialEq, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub enum Outcome {
    #[display("Success(Swapped)")]
    Buy,
    #[display("Failure(Refunded)")]
    Refund,
    #[display("Failure(Punished)")]
    Punish,
    #[display("Failure(Aborted)")]
    Abort,
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

#[derive(Clone, Debug, Eq, PartialEq, Display, NetworkEncode, NetworkDecode)]
#[display("{offer}, {status}")]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(OfferStatusPair::to_yaml_string)]
pub struct OfferStatusPair {
    pub offer: PublicOffer,
    pub status: OfferStatus,
}

#[cfg(feature = "serde")]
impl ToYamlString for OfferStatusPair {}

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

/// Information about server-side failure returned through RPC API
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[derive(
    Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Display, NetworkEncode, NetworkDecode,
)]
#[display("{info}", alt = "Server returned failure #{code}: {info}")]
pub struct Failure {
    /// Failure code
    pub code: FailureCode,

    /// Detailed information about the failure
    pub info: String,
}

#[derive(
    Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Hash, Debug, Display, NetworkEncode, NetworkDecode,
)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub enum FailureCode {
    /// Catch-all: TODO: Expand
    Unknown = 0xFFF,
}

impl From<u16> for FailureCode {
    fn from(value: u16) -> Self {
        match value {
            _ => FailureCode::Unknown,
        }
    }
}

impl From<FailureCode> for u16 {
    fn from(code: FailureCode) -> Self {
        code as u16
    }
}

impl From<FailureCode> for rpc::FailureCode<FailureCode> {
    fn from(code: FailureCode) -> Self {
        rpc::FailureCode::Other(code)
    }
}

impl rpc::FailureCodeExt for FailureCode {}

impl From<crate::Error> for BusMsg {
    fn from(err: crate::Error) -> Self {
        BusMsg::Ctl(Ctl::Failure(Failure {
            code: FailureCode::Unknown,
            info: err.to_string(),
        }))
    }
}

pub trait IntoProgressOrFailure {
    fn into_progress_or_failure(self) -> BusMsg;
}
pub trait IntoSuccessOrFailure {
    fn into_success_or_failure(self) -> BusMsg;
}

impl IntoProgressOrFailure for Result<String, crate::Error> {
    fn into_progress_or_failure(self) -> BusMsg {
        match self {
            Ok(val) => BusMsg::Ctl(Ctl::Progress(Progress::Message(val))),
            Err(err) => BusMsg::from(err),
        }
    }
}

impl IntoSuccessOrFailure for Result<String, crate::Error> {
    fn into_success_or_failure(self) -> BusMsg {
        match self {
            Ok(val) => BusMsg::Ctl(Ctl::Success(OptionDetails::with(val))),
            Err(err) => BusMsg::from(err),
        }
    }
}

impl IntoSuccessOrFailure for Result<(), crate::Error> {
    fn into_success_or_failure(self) -> BusMsg {
        match self {
            Ok(_) => BusMsg::Ctl(Ctl::Success(OptionDetails::new())),
            Err(err) => BusMsg::from(err),
        }
    }
}
