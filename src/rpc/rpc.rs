use farcaster_core::{swap::btcxmr::PublicOffer, swap::SwapId};

use amplify::ToYamlString;
use internet2::addr::{InetSocketAddr, NodeAddr};
use internet2::Api;
#[cfg(feature = "serde")]
use serde_with::{DisplayFromStr, DurationSeconds};
use std::time::Duration;
use strict_encoding::{StrictDecode, StrictEncode};

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

    //
    // RESPONSES
    //

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
    pub node_ids: Vec<internet2::addr::NodeId>,
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

#[cfg(feature = "serde")]
impl ToYamlString for NodeInfo {}
#[cfg(feature = "serde")]
impl ToYamlString for PeerInfo {}
#[cfg(feature = "serde")]
impl ToYamlString for SwapInfo {}
#[cfg(feature = "serde")]
impl ToYamlString for SyncerInfo {}
