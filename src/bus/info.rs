use std::str::FromStr;
use std::time::Duration;

use amplify::ToYamlString;
use farcaster_core::{blockchain::Blockchain, swap::btcxmr::PublicOffer, swap::SwapId};
use internet2::addr::{InetSocketAddr, NodeAddr};
#[cfg(feature = "serde")]
use serde_with::{DisplayFromStr, DurationSeconds};
use strict_encoding::{NetworkDecode, NetworkEncode, StrictDecode, StrictEncode};
use uuid::Uuid;

use crate::bus::{
    AddressSecretKey, CheckpointEntry, Failure, List, OfferStatusPair, OptionDetails, Progress,
};
use crate::cli::OfferSelector;
use crate::farcasterd::stats::Stats;
use crate::syncerd::runtime::SyncerdTask;

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

    #[display("list_offers({0})")]
    ListOffers(OfferStatusSelector),

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
    MadeOffer(MadeOffer),

    #[display(inner)]
    TookOffer(TookOffer),

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

    // - ListOffers section
    #[display(inner)]
    #[from]
    OfferList(List<OfferInfo>),

    #[display(inner)]
    OfferStatusList(List<OfferStatusPair>),
    // - End ListOffers section

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
    #[display("bitcoin_address_list({0})")]
    BitcoinAddressList(List<bitcoin::Address>),

    #[display("monero_address_list({0})")]
    MoneroAddressList(List<String>),
    // - End GetAddresses section

    // - GetCheckpointEntry section
    #[display("checkpoint_entry({0})")]
    CheckpointEntry(CheckpointEntry),
    // - End GetCheckpointEntry section
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(MadeOffer::to_yaml_string)]
pub struct MadeOffer {
    pub message: String,
    pub offer_info: OfferInfo,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(TookOffer::to_yaml_string)]
pub struct TookOffer {
    pub offerid: Uuid,
    pub message: String,
}

impl StrictEncode for TookOffer {
    fn strict_encode<W: std::io::Write>(&self, mut w: W) -> Result<usize, strict_encoding::Error> {
        let mut len = self.offerid.to_bytes_le().strict_encode(&mut w)?;
        len += self.message.strict_encode(&mut w)?;
        Ok(len)
    }
}

impl StrictDecode for TookOffer {
    fn strict_decode<R: std::io::Read>(mut r: R) -> Result<Self, strict_encoding::Error> {
        let offerid = Uuid::from_bytes_le(<[u8; 16]>::strict_decode(&mut r)?);
        let message = String::strict_decode(&mut r)?;
        Ok(TookOffer { offerid, message })
    }
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
    pub offers: Vec<PublicOffer>,
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
    #[serde_as(as = "Option<DisplayFromStr>")]
    pub swap_id: Option<SwapId>,
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub maker_peer: Vec<NodeAddr>,
    #[serde_as(as = "DurationSeconds")]
    pub uptime: Duration,
    pub since: u64,
    pub public_offer: PublicOffer,
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
    #[serde(rename = "transition")]
    StateTransition(String),
    #[serde(rename = "success")]
    Success(OptionDetails),
    #[serde(rename = "failure")]
    Failure(Failure),
}

#[derive(Eq, PartialEq, Clone, Debug, Display, NetworkDecode, NetworkEncode)]
pub enum Address {
    #[display("{0}")]
    Bitcoin(bitcoin::Address),
    #[display("{0}")]
    Monero(monero::Address),
}

#[derive(Clone, Debug, Eq, PartialEq, Display, NetworkEncode, NetworkDecode)]
pub enum OfferStatusSelector {
    #[display("Open")]
    Open,
    #[display("In Progress")]
    InProgress,
    #[display("Ended")]
    Ended,
    #[display("All")]
    All,
}

impl From<OfferSelector> for OfferStatusSelector {
    fn from(offer_selector: OfferSelector) -> OfferStatusSelector {
        match offer_selector {
            OfferSelector::Open => OfferStatusSelector::Open,
            OfferSelector::InProgress => OfferStatusSelector::InProgress,
            OfferSelector::Ended => OfferStatusSelector::Ended,
            OfferSelector::All => OfferStatusSelector::All,
        }
    }
}

impl FromStr for OfferStatusSelector {
    type Err = ();
    fn from_str(input: &str) -> Result<OfferStatusSelector, Self::Err> {
        match input {
            "open" | "Open" => Ok(OfferStatusSelector::Open),
            "in_progress" | "inprogress" => Ok(OfferStatusSelector::Open),
            "ended" | "Ended" => Ok(OfferStatusSelector::Ended),
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
#[display(OfferInfo::to_yaml_string)]
pub struct OfferInfo {
    pub offer: String,
    pub details: PublicOffer,
}

#[cfg(feature = "serde")]
impl ToYamlString for OfferInfo {}
#[cfg(feature = "serde")]
impl ToYamlString for MadeOffer {}
#[cfg(feature = "serde")]
impl ToYamlString for TookOffer {}
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
