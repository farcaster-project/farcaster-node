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

use crate::walletd::NodeSecrets;
use amplify::{ToYamlString, Wrapper};
use internet2::addr::InetSocketAddr;
#[cfg(feature = "serde")]
use serde_with::{DisplayFromStr, DurationSeconds, Same};
use std::fmt::{self, Debug, Display, Formatter};
use std::iter::FromIterator;
use std::time::Duration;
use std::{collections::BTreeMap, convert::TryInto};

use bitcoin::{secp256k1, OutPoint};
use farcaster_chains::{bitcoin::Bitcoin, monero::Monero, pairs::btcxmr::BtcXmr};
use farcaster_core::{
    blockchain::FeePolitic,
    bundle::{
        AliceParameters, BobParameters, CoreArbitratingTransactions, CosignedArbitratingCancel,
    },
    negotiation::{Offer, PublicOffer},
    protocol_message::{self, RevealAliceParameters, RevealBobParameters},
};
use internet2::Api;
use internet2::{NodeAddr, RemoteSocketAddr};
use lnp::payment::{self, AssetsBalance, Lifecycle};
use lnp::{message, ChannelId as SwapId, Messages, TempChannelId as TempSwapId};
use lnpbp::chain::AssetId;
use lnpbp::strict_encoding::{StrictDecode, StrictEncode};
use microservices::rpc::Failure;
use microservices::rpc_connection;
#[derive(Clone, Debug, Display, From, StrictDecode, StrictEncode, Api)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[api(encoding = "strict")]
pub enum Msg {
    #[api(type = 28)]
    #[display("maker_commit(...)")]
    MakerCommit(Commit),
    #[api(type = 20)]
    #[display("taker_commit(...)")]
    TakerCommit(TakeCommit),
    #[api(type = 22)]
    #[display("reveal(...)")]
    Reveal(Reveal),
    #[api(type = 25)]
    #[display("refunprocsig_a(...)")]
    RefundProcedureSignatures(protocol_message::RefundProcedureSignatures<BtcXmr>),
    #[api(type = 27)]
    #[display("abort(...)")]
    Abort(protocol_message::Abort),
    #[api(type = 24)]
    #[display("corearb_b(...)")]
    CoreArbitratingSetup(protocol_message::CoreArbitratingSetup<BtcXmr>),
    #[api(type = 26)]
    #[display("buyprocsig_b(...)")]
    BuyProcedureSignature(protocol_message::BuyProcedureSignature<BtcXmr>),
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("commit")]
pub enum Commit {
    Alice(CommitAliceParameters<BtcXmr>),
    Bob(CommitBobParameters<BtcXmr>),
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("nodeid")]
pub struct NodeId(pub bitcoin::secp256k1::PublicKey);

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("runtime context")]
pub enum RuntimeContext {
    GetInfo,
    ConnectPeer(NodeAddr),
    Listen(RemoteSocketAddr),
    MakeOffer(ProtoPublicOffer),
    TakeOffer(PublicOffer<BtcXmr>),
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("secret")]
pub struct Secret(pub NodeSecrets, pub Option<RuntimeContext>);

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("get secret")]
pub struct GetSecret(pub String, pub Option<RuntimeContext>);

#[derive(Clone, Debug, From, StrictDecode, StrictEncode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
pub struct TakeCommit {
    pub commit: Commit,
    pub public_offer_hex: String, // TODO: replace by public offer id
    pub swap_id: SwapId,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("reveal")]
pub enum Reveal {
    Alice(RevealAliceParameters<BtcXmr>),
    Bob(RevealBobParameters<BtcXmr>),
}

// #[cfg_attr(feature = "serde", serde_as)]
// #[cfg_attr(
//     feature = "serde",
//     derive(Serialize, Deserialize),
//     serde(crate = "serde_crate")
// )]
#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("params")]
pub enum Params {
    Alice(AliceParameters<BtcXmr>),
    Bob(BobParameters<BtcXmr>),
}

use crate::{Error, ServiceId};

#[derive(Clone, Debug, Display, From, Api)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[api(encoding = "strict")]
#[non_exhaustive]
pub enum Request {
    // Part I: Generic messages outside of channel operations
    // ======================================================
    /// Once authentication is complete, the first message reveals the features
    /// supported or required by this node, even if this is a reconnection.
    #[api(type = 16)]
    #[display(inner)]
    Init(message::Init),

    /// For simplicity of diagnosis, it's often useful to tell a peer that
    /// something is incorrect.
    #[api(type = 17)]
    #[display(inner)]
    Error(message::Error),

    /// In order to allow for the existence of long-lived TCP connections, at
    /// times it may be required that both ends keep alive the TCP connection
    /// at the application level. Such messages also allow obfuscation of
    /// traffic patterns.
    #[api(type = 18)]
    #[display(inner)]
    Ping(message::Ping),

    /// The pong message is to be sent whenever a ping message is received. It
    /// serves as a reply and also serves to keep the connection alive, while
    /// explicitly notifying the other end that the receiver is still active.
    /// Within the received ping message, the sender will specify the number of
    /// bytes to be included within the data payload of the pong message.
    #[api(type = 19)]
    #[display("pong(...)")]
    Pong(Vec<u8>),

    #[api(type = 0)]
    #[display("hello()")]
    Hello,

    #[api(type = 1)]
    #[display("update_channel_id({0})")]
    UpdateSwapId(SwapId),

    #[api(type = 2)]
    #[display("send_message({0})")]
    PeerMessage(Messages),

    #[api(type = 31)]
    #[display("getnodeid")]
    GetNodeId,

    #[api(type = 32)]
    #[display("nodeid")]
    NodeId(NodeId),

    #[api(type = 30)]
    #[display("getsecret")]
    GetSecret(GetSecret),

    #[api(type = 40)]
    #[display("loopback")]
    Loopback(RuntimeContext),

    #[api(type = 29)]
    #[display("secret")]
    Secret(Secret),

    #[api(type = 5)]
    #[display("send_message({0})")]
    Protocol(Msg),

    // Can be issued from `cli` to `lnpd`
    #[api(type = 100)]
    #[display("get_info()")]
    GetInfo,

    // Can be issued from `cli` to `lnpd`
    #[api(type = 101)]
    #[display("list_peers()")]
    ListPeers,

    // Can be issued from `cli` to `lnpd`
    #[api(type = 102)]
    #[display("list_swaps()")]
    ListSwaps,

    #[api(type = 103)]
    #[display("list_tasks()")]
    ListTasks,

    // Can be issued from `cli` to `lnpd`
    #[api(type = 200)]
    #[display("listen({0})")]
    Listen(RemoteSocketAddr),

    // Can be issued from `cli` to `lnpd`
    #[api(type = 201)]
    #[display("connect({0})")]
    ConnectPeer(NodeAddr),

    // Can be issued from `cli` to a specific `peerd`
    #[api(type = 202)]
    #[display("ping_peer()")]
    PingPeer,

    // Can be issued from `cli` to `lnpd`
    #[api(type = 203)]
    #[display("take_swap(...)")]
    TakeSwap(InitSwap),

    // Can be issued from `cli` to `lnpd`
    #[api(type = 204)]
    #[display("make_swap(...)")]
    MakeSwap(InitSwap),

    #[api(type = 199)]
    #[display("public_offer({0:#}))")]
    TakeOffer(PublicOffer<BtcXmr>),

    #[api(type = 198)]
    #[display("proto_puboffer({0:#})")]
    MakeOffer(ProtoPublicOffer),

    #[api(type = 197)]
    #[display("params({0:#})")]
    Params(Params),

    #[api(type = 205)]
    #[display("fund_swap({0})")]
    FundSwap(OutPoint),

    // Responses to CLI
    // ----------------
    #[api(type = 1002)]
    #[display("progress({0})")]
    Progress(String),

    #[api(type = 1001)]
    #[display("success({0})")]
    Success(OptionDetails),

    #[api(type = 1000)]
    #[display("failure({0:#})")]
    #[from]
    Failure(Failure),

    #[api(type = 1098)]
    #[display("public_offer_hex({0})")]
    PublicOfferHex(String),

    #[api(type = 1099)]
    #[display("syncer_info({0})", alt = "{0:#}")]
    #[from]
    SyncerInfo(SyncerInfo),

    #[api(type = 1100)]
    #[display("node_info({0})", alt = "{0:#}")]
    #[from]
    NodeInfo(NodeInfo),

    #[api(type = 1101)]
    #[display("node_info({0})", alt = "{0:#}")]
    #[from]
    PeerInfo(PeerInfo),

    #[api(type = 1102)]
    #[display("channel_info({0})", alt = "{0:#}")]
    #[from]
    SwapInfo(SwapInfo),

    #[api(type = 1103)]
    #[display("peer_list({0})", alt = "{0:#}")]
    #[from]
    PeerList(List<NodeAddr>),

    #[api(type = 1104)]
    #[display("swap_list({0})", alt = "{0:#}")]
    #[from]
    SwapList(List<SwapId>),

    #[api(type = 1105)]
    #[display("task_list({0})", alt = "{0:#}")]
    #[from]
    TaskList(List<u64>), // FIXME

    // #[api(type = 1203)]
    // #[display("channel_funding({0})", alt = "{0:#}")]
    // #[from]
    // SwapFunding(PubkeyScript),
    #[api(type = 1300)]
    #[display("task({0})", alt = "{0:#}")]
    #[from]
    CreateTask(u64), // FIXME
}

impl rpc_connection::Request for Request {}

use farcaster_core::protocol_message::{CommitAliceParameters, CommitBobParameters};
// use farcaster_chains::pairs::btcxmr::BtcXmr;

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("{peerd}, {swap_id}")]
pub struct InitSwap {
    pub peerd: ServiceId,
    pub report_to: Option<ServiceId>,
    pub params: Params,
    pub swap_id: SwapId,
    pub commit: Option<Commit>,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display(NodeInfo::to_yaml_string)]
pub struct NodeInfo {
    pub node_id: secp256k1::PublicKey,
    pub listens: Vec<RemoteSocketAddr>,
    #[serde_as(as = "DurationSeconds")]
    pub uptime: Duration,
    pub since: u64,
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub peers: Vec<NodeAddr>,
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub swaps: Vec<SwapId>,
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub offers: Vec<PublicOffer<BtcXmr>>,
}

#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("proto_puboffer")]
pub struct ProtoPublicOffer {
    pub offer: Offer<BtcXmr>,
    pub remote_addr: RemoteSocketAddr,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
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
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display(PeerInfo::to_yaml_string)]
pub struct PeerInfo {
    pub local_id: secp256k1::PublicKey,
    pub remote_id: Vec<secp256k1::PublicKey>,
    #[serde_as(as = "Option<DisplayFromStr>")]
    pub local_socket: Option<InetSocketAddr>,
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub remote_socket: Vec<InetSocketAddr>,
    #[serde_as(as = "DurationSeconds")]
    pub uptime: Duration,
    pub since: u64,
    pub messages_sent: usize,
    pub messages_received: usize,
    pub connected: bool,
    pub awaits_pong: bool,
}
pub type RemotePeerMap<T> = BTreeMap<NodeAddr, T>;
#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display(SwapInfo::to_yaml_string)]
pub struct SwapInfo {
    #[serde_as(as = "Option<DisplayFromStr>")]
    pub swap_id: Option<SwapId>,
    // pub state: crate::swapd::State,
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub assets: Vec<AssetId>,
    // #[serde_as(as = "BTreeMap<DisplayFromStr, Same>")]
    // pub funding_outpoint: OutPoint,
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub remote_peers: Vec<NodeAddr>,
    #[serde_as(as = "DurationSeconds")]
    pub uptime: Duration,
    pub since: u64,
    pub is_originator: bool,
    //// FIXME serde::Serialize/Deserialize missing
    // #[serde_as(as = "Option<DisplayFromStr)>")]
    // pub params: Option<Params>,
    pub local_keys: payment::channel::Keyset,
    #[serde_as(as = "BTreeMap<DisplayFromStr, Same>")]
    pub remote_keys: BTreeMap<NodeAddr, payment::channel::Keyset>,
}

#[cfg(feature = "serde")]
impl ToYamlString for NodeInfo {}
#[cfg(feature = "serde")]
impl ToYamlString for PeerInfo {}
#[cfg(feature = "serde")]
impl ToYamlString for SwapInfo {}
#[cfg(feature = "serde")]
impl ToYamlString for SyncerInfo {}

#[derive(Wrapper, Clone, PartialEq, Eq, Debug, From, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[wrapper(IndexRange)]
pub struct List<T>(Vec<T>)
where
    T: Clone + PartialEq + Eq + Debug + Display + StrictEncode + StrictDecode;

#[cfg(feature = "serde")]
impl<'a, T> Display for List<T>
where
    T: Clone + PartialEq + Eq + Debug + Display + serde::Serialize + StrictEncode + StrictDecode,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&serde_yaml::to_string(self).expect("internal YAML serialization error"))
    }
}

impl<T> FromIterator<T> for List<T>
where
    T: Clone + PartialEq + Eq + Debug + Display + serde::Serialize + StrictEncode + StrictDecode,
{
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::from_inner(iter.into_iter().collect())
    }
}

#[cfg(feature = "serde")]
impl<T> serde::Serialize for List<T>
where
    T: Clone + PartialEq + Eq + Debug + Display + serde::Serialize + StrictEncode + StrictDecode,
{
    fn serialize<S>(
        &self,
        serializer: S,
    ) -> Result<<S as serde::Serializer>::Ok, <S as serde::Serializer>::Error>
    where
        S: serde::Serializer,
    {
        self.as_inner().serialize(serializer)
    }
}

#[derive(Wrapper, Clone, PartialEq, Eq, Debug, From, Default, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
pub struct OptionDetails(pub Option<String>);

impl Display for OptionDetails {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.as_inner() {
            None => Ok(()),
            Some(msg) => f.write_str(&msg),
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

impl From<crate::Error> for Request {
    fn from(err: crate::Error) -> Self {
        Request::Failure(Failure::from(err))
    }
}

pub trait IntoProgressOrFalure {
    fn into_progress_or_failure(self) -> Request;
}
pub trait IntoSuccessOrFalure {
    fn into_success_or_failure(self) -> Request;
}

impl IntoProgressOrFalure for Result<String, crate::Error> {
    fn into_progress_or_failure(self) -> Request {
        match self {
            Ok(val) => Request::Progress(val),
            Err(err) => Request::from(err),
        }
    }
}

impl IntoSuccessOrFalure for Result<String, crate::Error> {
    fn into_success_or_failure(self) -> Request {
        match self {
            Ok(val) => Request::Success(OptionDetails::with(val)),
            Err(err) => Request::from(err),
        }
    }
}

impl IntoSuccessOrFalure for Result<(), crate::Error> {
    fn into_success_or_failure(self) -> Request {
        match self {
            Ok(_) => Request::Success(OptionDetails::new()),
            Err(err) => Request::from(err),
        }
    }
}

impl From<Params> for Commit {
    fn from(params: Params) -> Self {
        match params {
            Params::Alice(params) => {
                let params: CommitAliceParameters<BtcXmr> = params.into();
                Commit::Alice(params)
            }
            Params::Bob(params) => {
                let params: CommitBobParameters<BtcXmr> = params.into();
                Commit::Bob(params)
            }
        }
    }
}

impl TryInto<Reveal> for Params {
    type Error = farcaster_core::Error;

    fn try_into(self) -> Result<Reveal, Self::Error> {
        match self {
            Params::Alice(params) => {
                let params: RevealAliceParameters<BtcXmr> = params.try_into()?;
                Ok(Reveal::Alice(params))
            }

            Params::Bob(params) => {
                let params: RevealBobParameters<BtcXmr> = params.try_into()?;
                Ok(Reveal::Bob(params))
            }
        }
    }
}
