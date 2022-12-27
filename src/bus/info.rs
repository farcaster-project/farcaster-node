// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use std::str::FromStr;
use std::time::Duration;

use amplify::ToYamlString;
use farcaster_core::role::{SwapRole, TradeRole};
use farcaster_core::trade::DealId;
use farcaster_core::{blockchain::Blockchain, swap::btcxmr::Deal, swap::SwapId};
use internet2::addr::{InetSocketAddr, NodeAddr, NodeId};
#[cfg(feature = "serde")]
use serde_with::{DisplayFromStr, DurationSeconds};
use strict_encoding::{NetworkDecode, NetworkEncode};

use crate::bus::{
    AddressSecretKey, CheckpointEntry, DealInfo, Failure, List, OptionDetails, Progress,
};
use crate::cli::DealSelector;
use crate::farcasterd::stats::Stats;
use crate::swapd::StateReport;
use crate::syncerd::runtime::SyncerdTask;
use crate::Error;

use super::ctl::FundingInfo;
use super::StateTransition;

#[derive(Clone, Debug, Display, From, NetworkEncode, NetworkDecode)]
#[non_exhaustive]
pub enum InfoMsg {
    //
    // QUERIES
    //
    #[display("get_info()")]
    GetInfo,

    #[display("list_peers()")]
    ListPeers,

    #[display("list_swaps()")]
    ListSwaps,

    #[display("list_tasks()")]
    ListTasks,

    #[display("list_deals({0})")]
    ListDeals(DealStatusSelector),

    #[display("list_listens()")]
    ListListens,

    #[display("retrieve_all_checkpoint_info")]
    RetrieveAllCheckpointInfo,

    #[display("get_address_secret_key({0})")]
    GetAddressSecretKey(Address),

    #[display("get_addresses({0})")]
    GetAddresses(Blockchain),

    #[display("needs_funding({0})")]
    NeedsFunding(Blockchain),

    #[display("get_checkpoint_entry({0})")]
    GetCheckpointEntry(SwapId),

    // Progress functionalities
    // ----------------
    // Returns a SwapProgress message
    #[display("read_progress({0})")]
    ReadProgress(SwapId),

    #[display(inner)]
    SwapProgress(SwapProgress),

    #[display("subscribe_progress({0})")]
    SubscribeProgress(SwapId),

    #[display("unsubscribe_progress({0})")]
    UnsubscribeProgress(SwapId),

    #[display(inner)]
    Progress(Progress),

    #[display(inner)]
    Success(OptionDetails),

    #[display(inner)]
    #[from]
    Failure(Failure),

    //
    // RESPONSES
    //
    #[display(inner)]
    String(String),

    #[display(inner)]
    MadeDeal(MadeDeal),

    #[display(inner)]
    TookDeal(TookDeal),

    // - GetInfo section
    #[display("syncer_info(..)")]
    #[from]
    SyncerInfo(SyncerInfo),

    #[display("node_info(..)")]
    #[from]
    NodeInfo(NodeInfo),

    #[display("peer_info(..)")]
    #[from]
    PeerInfo(PeerInfo),

    #[display("swap_info(..)")]
    #[from]
    SwapInfo(SwapInfo),
    // - End GetInfo section

    // - ListPeers section
    #[display(inner)]
    #[from]
    PeerList(List<NodeAddr>),
    // - End ListPeers section

    // - ListSwap section
    #[display(inner)]
    #[from]
    SwapList(List<SwapId>),
    // - End ListSwap section

    // - ListTasks section
    #[display(inner)]
    #[from]
    TaskList(List<SyncerdTask>),
    // - End ListTasks section

    // - ListDeals section
    #[display(inner)]
    #[from]
    DealList(List<DealInfo>),

    #[display(inner)]
    DealInfoList(List<DealInfo>),
    // - End ListDeals section

    // - ListListen section
    #[display(inner)]
    #[from]
    ListenList(List<String>),
    // - End ListListen section
    #[display(inner)]
    CheckpointList(List<CheckpointEntry>),

    // - GetAddressSecretKey section
    #[display("address_secret_key")]
    AddressSecretKey(AddressSecretKey),
    // - End GetAddressSecretKey section

    // - GetAddresses section
    #[display(inner)]
    BitcoinAddressList(List<BitcoinAddressSwapIdPair>),

    #[display(inner)]
    MoneroAddressList(List<MoneroAddressSwapIdPair>),
    // - End GetAddresses section

    // - GetCheckpointEntry section
    #[display("checkpoint_entry({0})")]
    CheckpointEntry(CheckpointEntry),
    // - End GetCheckpointEntry section
    #[display("{0}")]
    FundingInfos(FundingInfos),

    #[display("{0}")]
    AddressBalance(AddressBalance),
}

#[derive(Clone, PartialEq, Eq, Debug, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(BitcoinAddressSwapIdPair::to_yaml_string)]
pub struct BitcoinAddressSwapIdPair {
    pub address: bitcoin::Address,
    pub swap_id: Option<SwapId>,
}

#[derive(Clone, PartialEq, Eq, Debug, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(MoneroAddressSwapIdPair::to_yaml_string)]
pub struct MoneroAddressSwapIdPair {
    pub address: monero::Address,
    pub swap_id: Option<SwapId>,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(MadeDeal::to_yaml_string)]
pub struct MadeDeal {
    pub message: String,
    pub viewable_deal: ViewableDeal,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(TookDeal::to_yaml_string)]
pub struct TookDeal {
    pub deal_id: DealId,
    pub message: String,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(SyncerInfo::to_yaml_string)]
pub struct SyncerInfo {
    pub syncer: String,
    #[serde_as(as = "DurationSeconds")]
    pub uptime: Duration,
    pub since: u64,
    pub tasks: Vec<SyncerdTask>,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(NodeInfo::to_yaml_string)]
pub struct NodeInfo {
    pub listens: Vec<InetSocketAddr>,
    #[serde_as(as = "DurationSeconds")]
    pub uptime: Duration,
    pub since: u64,
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub peers: Vec<NodeAddr>,
    pub swaps: Vec<SwapId>,
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub deals: Vec<Deal>,
    #[serde(alias = "statistics")]
    pub stats: Stats,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(PeerInfo::to_yaml_string)]
pub struct PeerInfo {
    pub local_id: internet2::addr::NodeId,
    pub remote_id: Vec<internet2::addr::NodeId>,
    #[serde_as(as = "Option<DisplayFromStr>")]
    pub local_socket: Option<InetSocketAddr>,
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub remote_socket: Vec<InetSocketAddr>,
    #[serde_as(as = "DurationSeconds")]
    pub uptime: Duration,
    pub since: u64,
    pub messages_sent: usize,
    pub messages_received: usize,
    pub forked_from_listener: bool,
    pub awaits_pong: bool,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(SwapInfo::to_yaml_string)]
pub struct SwapInfo {
    pub swap_id: SwapId,
    #[serde_as(as = "Option<DisplayFromStr>")]
    pub connection: Option<NodeAddr>,
    pub connected: bool,
    #[serde_as(as = "DurationSeconds")]
    pub uptime: Duration,
    pub since: u64,
    pub deal: Deal,
    pub local_trade_role: TradeRole,
    pub local_swap_role: SwapRole,
    pub connected_counterparty_node_id: Option<NodeId>,
    pub state: StateReport,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, Default, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(SwapProgress::to_yaml_string)]
pub struct SwapProgress {
    pub progress: Vec<ProgressEvent>,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, Default, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(FundingInfos::to_yaml_string)]
pub struct FundingInfos {
    pub swaps_need_funding: Vec<FundingInfo>,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(ProgressEvent::to_yaml_string)]
pub enum ProgressEvent {
    #[serde(rename = "message")]
    Message(String),
    #[serde(rename = "update")]
    StateUpdate(StateReport),
    #[serde(rename = "transition")]
    StateTransition(StateTransition),
    #[serde(rename = "success")]
    Success(OptionDetails),
    #[serde(rename = "failure")]
    Failure(Failure),
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Eq, PartialEq, Clone, Debug, Display, Hash, NetworkDecode, NetworkEncode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub enum Address {
    #[display("{0}")]
    Bitcoin(bitcoin::Address),
    #[display("{0}")]
    Monero(monero::Address),
}

impl FromStr for Address {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        bitcoin::Address::from_str(s)
            .map(Address::Bitcoin)
            .map_err(Error::from)
            .or_else(|_| {
                monero::Address::from_str(s)
                    .map(Address::Monero)
                    .map_err(Error::from)
            })
    }
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Eq, PartialEq, Clone, Debug, Display, Hash, NetworkDecode, NetworkEncode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(AddressBalance::to_yaml_string)]
pub struct AddressBalance {
    pub address: Address,
    pub balance: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Display, NetworkEncode, NetworkDecode)]
pub enum DealStatusSelector {
    #[display("Open")]
    Open,
    #[display("In Progress")]
    InProgress,
    #[display("Ended")]
    Ended,
    #[display("All")]
    All,
}

impl From<DealSelector> for DealStatusSelector {
    fn from(deal_selector: DealSelector) -> DealStatusSelector {
        match deal_selector {
            DealSelector::Open => DealStatusSelector::Open,
            DealSelector::InProgress => DealStatusSelector::InProgress,
            DealSelector::Ended => DealStatusSelector::Ended,
            DealSelector::All => DealStatusSelector::All,
        }
    }
}

impl FromStr for DealStatusSelector {
    type Err = ();
    fn from_str(input: &str) -> Result<DealStatusSelector, Self::Err> {
        match input {
            "open" | "Open" => Ok(DealStatusSelector::Open),
            "in_progress" | "inprogress" => Ok(DealStatusSelector::Open),
            "ended" | "Ended" => Ok(DealStatusSelector::Ended),
            _ => Err(()),
        }
    }
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(ViewableDeal::to_yaml_string)]
pub struct ViewableDeal {
    pub deal: String,
    pub details: Deal,
}

#[cfg(feature = "serde")]
impl ToYamlString for BitcoinAddressSwapIdPair {}
#[cfg(feature = "serde")]
impl ToYamlString for MoneroAddressSwapIdPair {}
#[cfg(feature = "serde")]
impl ToYamlString for ViewableDeal {}
#[cfg(feature = "serde")]
impl ToYamlString for MadeDeal {}
#[cfg(feature = "serde")]
impl ToYamlString for TookDeal {}
#[cfg(feature = "serde")]
impl ToYamlString for CheckpointEntry {}
#[cfg(feature = "serde")]
impl ToYamlString for SwapProgress {}
#[cfg(feature = "serde")]
impl ToYamlString for NodeInfo {}
#[cfg(feature = "serde")]
impl ToYamlString for PeerInfo {}
#[cfg(feature = "serde")]
impl ToYamlString for SwapInfo {}
#[cfg(feature = "serde")]
impl ToYamlString for SyncerInfo {}
#[cfg(feature = "serde")]
impl ToYamlString for ProgressEvent {}
#[cfg(feature = "serde")]
impl ToYamlString for FundingInfos {}
#[cfg(feature = "serde")]
impl ToYamlString for AddressBalance {}
