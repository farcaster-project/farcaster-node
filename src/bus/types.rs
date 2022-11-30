use std::fmt::{self, Debug, Display, Formatter};

use farcaster_core::{
    role::TradeRole,
    swap::{btcxmr::PublicOffer, SwapId},
};

use amplify::{ToYamlString, Wrapper};
use internet2::addr::NodeId;
use microservices::rpc;
use strict_encoding::{NetworkDecode, NetworkEncode};

#[derive(Clone, PartialEq, Eq, Debug, Display, NetworkEncode, NetworkDecode)]
#[display("{swap_id}, {public_offer}")]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(CheckpointEntry::to_yaml_string)]
pub struct CheckpointEntry {
    pub swap_id: SwapId,
    pub public_offer: PublicOffer,
    pub trade_role: TradeRole,
    pub expected_counterparty_node_id: Option<NodeId>,
}

#[derive(Clone, Debug, Display, NetworkDecode, NetworkEncode)]
pub enum AddressSecretKey {
    #[display("addr_key({address}, ..)")]
    Bitcoin {
        address: bitcoin::Address,
        secret_key: bitcoin::secp256k1::SecretKey,
    },
    #[display("addr_key({address}, ..)")]
    Monero {
        address: monero::Address,
        view: monero::PrivateKey,
        spend: monero::PrivateKey,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub enum OfferStatus {
    #[display("Open")]
    Open,
    #[display("In Progress")]
    InProgress,
    #[display("Ended({0})")]
    Ended(Outcome),
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

    TargetServiceNotFound = 0xFFE,
}

impl From<u16> for FailureCode {
    fn from(value: u16) -> Self {
        match value {
            0xFFE => FailureCode::TargetServiceNotFound,
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
