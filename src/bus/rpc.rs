use std::str::FromStr;
use std::time::Duration;

use farcaster_core::{
    blockchain::Blockchain, role::TradeRole, swap::btcxmr::PublicOffer, swap::SwapId,
};
use amplify::{ToYamlString};
use internet2::addr::{InetSocketAddr, NodeAddr};
use internet2::Api;
#[cfg(feature = "serde")]
use serde_with::{DisplayFromStr, DurationSeconds};
use strict_encoding::{StrictDecode, StrictEncode};
use uuid::Uuid;

use crate::bus::ctl::{OfferStatusPair, Progress, Outcome, Failure, OptionDetails};
use crate::bus::request::{List};
use crate::cli::OfferSelector;

#[derive(Clone, Debug, Display, From, StrictDecode, StrictEncode, Api)]
#[api(encoding = "strict")]
#[non_exhaustive]
pub enum Rpc {
    //
    // QUERIES
    //
    #[api(type = 100)]
    #[display("get_info()")]
    GetInfo,

    #[api(type = 101)]
    #[display("list_peers()")]
    ListPeers,

    #[api(type = 102)]
    #[display("list_swaps()")]
    ListSwaps,

    #[api(type = 103)]
    #[display("list_tasks()")]
    ListTasks,

    #[api(type = 104)]
    #[display("list_offers({0})")]
    ListOffers(OfferStatusSelector),

    #[api(type = 105)]
    #[display("list_listens()")]
    ListListens,

    #[api(type = 1306)]
    #[display("retrieve_all_checkpoint_info")]
    RetrieveAllCheckpointInfo,

    #[api(type = 1311)]
    #[display("get_address_secret_key({0})")]
    GetAddressSecretKey(Address),

    #[api(type = 1312)]
    #[display("get_addresses({0})")]
    GetAddresses(Blockchain),

    #[api(type = 1109)]
    #[display("needs_funding({0})")]
    NeedsFunding(Blockchain),

    // Progress functionalities
    // ----------------
    // Returns a SwapProgress message
    #[api(type = 1003)]
    #[display("read_progress({0})")]
    ReadProgress(SwapId),

    #[api(type = 1005)]
    #[display(inner)]
    SwapProgress(SwapProgress),

    #[api(type = 1006)]
    #[display("subscribe_progress({0})")]
    SubscribeProgress(SwapId),

    #[api(type = 1007)]
    #[display("unsubscribe_progress({0})")]
    UnsubscribeProgress(SwapId),

    #[api(type = 1002)]
    #[display(inner)]
    Progress(Progress),

    #[api(type = 1001)]
    #[display(inner)]
    Success(OptionDetails),

    #[api(type = 1000)]
    #[display(inner)]
    #[from]
    Failure(Failure),

    //
    // RESPONSES
    //
    #[api(type = 1004)]
    #[display(inner)]
    String(String),

    #[api(type = 206)]
    #[display(inner)]
    MadeOffer(MadeOffer),

    #[api(type = 207)]
    #[display(inner)]
    TookOffer(TookOffer),

    // - GetInfo section
    #[api(type = 1099)]
    #[display("syncer_info(..)")]
    #[from]
    SyncerInfo(SyncerInfo),

    #[api(type = 1100)]
    #[display("node_info(..)")]
    #[from]
    NodeInfo(NodeInfo),

    #[api(type = 1101)]
    #[display("peer_info(..)")]
    #[from]
    PeerInfo(PeerInfo),

    #[api(type = 1102)]
    #[display("swap_info(..)")]
    #[from]
    SwapInfo(SwapInfo),
    // - End GetInfo section

    // - ListPeers section
    #[api(type = 1103)]
    #[display(inner)]
    #[from]
    PeerList(List<NodeAddr>),
    // - End ListPeers section

    // - ListSwap section
    #[api(type = 1104)]
    #[display(inner)]
    #[from]
    SwapList(List<SwapId>),
    // - End ListSwap section

    // - ListTasks section
    #[api(type = 1105)]
    #[display(inner)]
    #[from]
    TaskList(List<u64>),
    // - End ListTasks section

    // - ListOffers section
    #[api(type = 1106)]
    #[display(inner)]
    #[from]
    OfferList(List<OfferInfo>),

    #[api(type = 1317)]
    #[display(inner)]
    OfferStatusList(List<OfferStatusPair>),
    // - End ListOffers section

    // - ListListen section
    #[api(type = 1107)]
    #[display(inner)]
    #[from]
    ListenList(List<String>),
    // - End ListListen section
    #[api(type = 1308)]
    #[display(inner)]
    CheckpointList(List<CheckpointEntry>),

    // - GetAddressSecretKey section
    #[api(type = 1319)]
    #[display("address_secret_key")]
    AddressSecretKey(AddressSecretKey),
    // - End GetAddressSecretKey section

    // - GetAddresses section
    #[api(type = 1313)]
    #[display("bitcoin_address_list({0})")]
    BitcoinAddressList(List<bitcoin::Address>),

    #[api(type = 1318)]
    #[display("monero_address_list({0})")]
    MoneroAddressList(List<String>),
    // - End GetAddresses section
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
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
#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(SyncerInfo::to_yaml_string)]
pub struct SyncerInfo {
    #[serde_as(as = "DurationSeconds")]
    pub uptime: Duration,
    pub since: u64,
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub tasks: Vec<u64>,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
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
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
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
#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, PartialEq, Eq)]
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

#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
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
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, Default, StrictEncode, StrictDecode)]
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
#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
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

#[derive(Eq, PartialEq, Clone, Debug, Display, StrictDecode, StrictEncode)]
pub enum Address {
    #[display("{0}")]
    Bitcoin(bitcoin::Address),
    #[display("{0}")]
    Monero(monero::Address),
}

#[derive(Clone, Debug, Display, StrictDecode, StrictEncode)]
#[display("address_secret_key")]
pub enum AddressSecretKey {
    Bitcoin {
        address: bitcoin::Address,
        secret_key: bitcoin::secp256k1::SecretKey,
    },
    Monero {
        address: monero::Address,
        view: monero::PrivateKey,
        spend: monero::PrivateKey,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Display, StrictEncode, StrictDecode)]
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

#[derive(Clone, Debug, Eq, PartialEq, Display, StrictEncode, StrictDecode)]
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

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
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
