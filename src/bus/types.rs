// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use std::{
    convert::TryInto,
    fmt::{self, Debug, Display, Formatter},
    str::FromStr,
};

use farcaster_core::{
    blockchain::Network,
    role::TradeRole,
    swap::{btcxmr::Deal, SwapId},
};

use amplify::{ToYamlString, Wrapper};
use internet2::addr::{InetSocketAddr, NodeId};
use microservices::rpc;
use serde_with::DisplayFromStr;
use strict_encoding::{NetworkDecode, NetworkEncode, StrictDecode, StrictEncode};
use torut::onion::{OnionAddressV3, TorPublicKeyV3};

use crate::swapd::StateReport;
use crate::syncerd::Health;

#[derive(Clone, PartialEq, Eq, Debug, Display, NetworkEncode, NetworkDecode)]
#[display("{swap_id}, {deal}")]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(CheckpointEntry::to_yaml_string)]
pub struct CheckpointEntry {
    pub swap_id: SwapId,
    pub deal: Deal,
    pub trade_role: TradeRole,
    pub expected_counterparty_node_id: Option<NodeId>,
}

#[derive(Clone, Debug, Display, Eq, PartialEq, NetworkDecode, NetworkEncode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub struct HiddenServiceInfo {
    pub onion_address: WrapOnionAddressV3,
    pub bind_address: InetSocketAddr,
}

#[derive(Clone, PartialEq, Eq, Debug, Display)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub struct WrapOnionAddressV3(pub OnionAddressV3);

impl StrictEncode for WrapOnionAddressV3 {
    fn strict_encode<E: std::io::Write>(&self, mut e: E) -> Result<usize, strict_encoding::Error> {
        let address_encoding = self.0.get_public_key().as_bytes().to_vec();
        address_encoding.strict_encode(&mut e)
    }
}

impl StrictDecode for WrapOnionAddressV3 {
    fn strict_decode<D: std::io::Read>(mut d: D) -> Result<Self, strict_encoding::Error> {
        let onion_address = Vec::<u8>::strict_decode(&mut d)?;
        Ok(WrapOnionAddressV3(OnionAddressV3::from(
            &TorPublicKeyV3::from_bytes(&onion_address.try_into().unwrap()).unwrap(),
        )))
    }
}

#[derive(Clone, Debug, Display, Eq, PartialEq, Hash, NetworkDecode, NetworkEncode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub enum AddressSecretKey {
    #[display("addr_key({address}, ..)")]
    Bitcoin {
        address: bitcoin::Address,
        secret_key_info: BitcoinSecretKeyInfo,
    },
    #[display("addr_key({address}, ..)")]
    Monero {
        address: monero::Address,
        secret_key_info: MoneroSecretKeyInfo,
    },
}

#[derive(Clone, Debug, Display, Eq, PartialEq, Hash, NetworkDecode, NetworkEncode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display("bitcoin secret key info")]
pub struct BitcoinSecretKeyInfo {
    pub swap_id: Option<SwapId>,
    pub secret_key: bitcoin::secp256k1::SecretKey,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, Debug, Display, Eq, PartialEq, Hash, NetworkDecode, NetworkEncode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display("monero secret key info")]
pub struct MoneroSecretKeyInfo {
    pub swap_id: Option<SwapId>,
    #[serde_as(as = "DisplayFromStr")]
    pub view: monero::PrivateKey,
    #[serde_as(as = "DisplayFromStr")]
    pub spend: monero::PrivateKey,
    pub creation_height: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub enum DealStatus {
    #[display("Open")]
    Open,
    #[display("In Progress")]
    InProgress,
    #[display("Revoked")]
    Revoked,
    #[display("Ended({0})")]
    Ended(Outcome),
}

#[derive(Clone, Debug, Eq, PartialEq, Display, NetworkEncode, NetworkDecode)]
#[display("{deal}, {status}")]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(DealInfo::to_yaml_string)]
pub struct DealInfo {
    pub deal: Deal,
    pub serialized_deal: String,
    pub status: DealStatus,
    pub local_trade_role: TradeRole,
}

#[cfg(feature = "serde")]
impl ToYamlString for DealInfo {}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub enum Outcome {
    #[display("Success Swap")]
    SuccessSwap,
    #[display("Failure Refund")]
    FailureRefund,
    #[display("Failure Punish")]
    FailurePunish,
    #[display("Failure Abort")]
    FailureAbort,
}

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[display(inner)]
pub enum Progress {
    Message(String),
    StateUpdate(StateReport),
    StateTransition(StateTransition),
}

#[derive(Clone, Debug, Eq, PartialEq, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display("State transition: {old_state} -> {new_state}")]
pub struct StateTransition {
    pub old_state: StateReport,
    pub new_state: StateReport,
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

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Display, From)]
pub enum HealthCheckSelector {
    #[display(inner)]
    Network(Network),
    #[display("all")]
    All,
}

impl FromStr for HealthCheckSelector {
    type Err = farcaster_core::consensus::Error;
    fn from_str(input: &str) -> Result<HealthCheckSelector, Self::Err> {
        match Network::from_str(input) {
            Ok(n) => Ok(HealthCheckSelector::Network(n)),
            Err(err) => {
                if input == "all" || input == "All" {
                    Ok(HealthCheckSelector::All)
                } else {
                    Err(err)
                }
            }
        }
    }
}

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(DefaultHealthReport::to_yaml_string)]
pub struct DefaultHealthReport {
    pub bitcoin_mainnet_health: Health,
    pub bitcoin_testnet_health: Health,
    pub monero_mainnet_health: Health,
    pub monero_testnet_health: Health,
}

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(CompleteHealthReport::to_yaml_string)]
pub struct CompleteHealthReport {
    pub bitcoin_mainnet_health: Health,
    pub bitcoin_testnet_health: Health,
    pub bitcoin_local_health: Health,
    pub monero_mainnet_health: Health,
    pub monero_testnet_health: Health,
    pub monero_local_health: Health,
}

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(ReducedHealthReport::to_yaml_string)]
pub struct ReducedHealthReport {
    pub bitcoin_health: Health,
    pub monero_health: Health,
}

#[cfg(feature = "serde")]
impl ToYamlString for DefaultHealthReport {}
#[cfg(feature = "serde")]
impl ToYamlString for CompleteHealthReport {}
#[cfg(feature = "serde")]
impl ToYamlString for ReducedHealthReport {}
