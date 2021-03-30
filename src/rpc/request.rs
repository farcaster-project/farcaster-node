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

use amplify::{ToYamlString, Wrapper};
use internet2::addr::InetSocketAddr;
#[cfg(feature = "serde")]
use serde_with::{DisplayFromStr, DurationSeconds, Same};
use std::collections::BTreeMap;
use std::fmt::{self, Debug, Display, Formatter};
use std::iter::FromIterator;
use std::time::Duration;

use bitcoin::{secp256k1, OutPoint};
use internet2::{NodeAddr, RemoteSocketAddr};
use internet2::Api;
use lnp::payment::{self, AssetsBalance, Lifecycle};
use lnp::{
    message, ChannelId as SwapId, Messages, TempChannelId as TempSwapId,
};
use lnpbp::chain::AssetId;
use lnpbp::strict_encoding::{StrictDecode, StrictEncode};
use microservices::rpc::Failure;
use microservices::rpc_connection;
use wallet::PubkeyScript;
use farcaster_core::{
    bitcoin::Bitcoin, monero::Monero,
    protocol_message,
};

#[derive(Clone, Debug, Display, From, StrictDecode, StrictEncode, Api)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[api(encoding = "strict")]
pub enum ProtocolMessagesAlice {
    #[api(type = 20)]
    #[display("commit_a(...)")]
    CommitAliceSessionParams(protocol_message::CommitAliceSessionParams<Bitcoin, Monero>),
    #[api(type = 22)]
    #[display("reveal_a(...)")]
    RevealAliceSessionParams(protocol_message::RevealAliceSessionParams<Bitcoin, Monero>),
    #[api(type = 25)]
    #[display("refunprocsig_a(...)")]
    RefundProcedureSignatures(protocol_message::RefundProcedureSignatures<Bitcoin>),
    #[api(type = 27)]
    #[display("abort(...)")]
    Abort(protocol_message::Abort),
}

#[derive(Clone, Debug, Display, From, StrictDecode, StrictEncode, Api)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[api(encoding = "strict")]
pub enum ProtocolMessagesBob {
    #[api(type = 21)]
    #[display("commit_b(...)")]
    CommitBobSessionParams(protocol_message::CommitBobSessionParams<Bitcoin, Monero>),
    #[api(type = 23)]
    #[display("reveal_b(...)")]
    RevealBobSessionParams(protocol_message::RevealBobSessionParams<Bitcoin, Monero>),
    #[api(type = 24)]
    #[display("corearb_b(...)")]
    CoreArbitratingSetup(protocol_message::CoreArbitratingSetup<Bitcoin>),
    #[api(type = 26)]
    #[display("buyprocsig_b(...)")]
    BuyProcedureSignature(protocol_message::BuyProcedureSignature<Bitcoin>),
}


use crate::ServiceId;

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

    #[api(type = 3)]
    #[display("send_message({0})")]
    ProtocolMessagesAlice(ProtocolMessagesAlice),

    #[api(type = 4)]
    #[display("send_message({0})")]
    ProtocolMessagesBob(ProtocolMessagesBob),

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
    #[display("create_channel_with(...)")]
    OpenSwapWith(CreateSwap),

    #[api(type = 204)]
    #[display("accept_channel_from(...)")]
    AcceptSwapFrom(CreateSwap),

    #[api(type = 205)]
    #[display("fund_channel({0})")]
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

    #[api(type = 1203)]
    #[display("channel_funding({0})", alt = "{0:#}")]
    #[from]
    SwapFunding(PubkeyScript),

    #[api(type = 1300)]
    #[display("task({0})", alt = "{0:#}")]
    #[from]
    CreateTask(u64), // FIXME

}

impl rpc_connection::Request for Request {}

#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("{peerd}, ...")]
pub struct CreateSwap {
    pub swap_req: message::OpenChannel,
    pub peerd: ServiceId,
    pub report_to: Option<ServiceId>,
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
    pub syncer_id: secp256k1::PublicKey,
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

//#[serde_as]
#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display(SwapInfo::to_yaml_string)]
pub struct SwapInfo {
    #[serde_as(as = "Option<DisplayFromStr>")]
    pub channel_id: Option<SwapId>,
    #[serde_as(as = "DisplayFromStr")]
    pub temporary_channel_id: TempSwapId,
    pub state: Lifecycle,
    pub local_capacity: u64,
    #[serde_as(as = "BTreeMap<DisplayFromStr, Same>")]
    pub remote_capacities: RemotePeerMap<u64>,
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub assets: Vec<AssetId>,
    #[serde_as(as = "BTreeMap<DisplayFromStr, Same>")]
    pub local_balances: AssetsBalance,
    #[serde_as(
        as = "BTreeMap<DisplayFromStr, BTreeMap<DisplayFromStr, Same>>"
    )]
    pub remote_balances: RemotePeerMap<AssetsBalance>,
    pub funding_outpoint: OutPoint,
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub remote_peers: Vec<NodeAddr>,
    #[serde_as(as = "DurationSeconds")]
    pub uptime: Duration,
    pub since: u64,
    pub commitment_updates: u64,
    pub total_payments: u64,
    pub pending_payments: u16,
    pub is_originator: bool,
    pub params: payment::channel::Params,
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


#[derive(
    Wrapper, Clone, PartialEq, Eq, Debug, From, StrictEncode, StrictDecode,
)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[wrapper(IndexRange)]
pub struct List<T>(Vec<T>)
where
    T: Clone + PartialEq + Eq + Debug + Display + StrictEncode + StrictDecode;

#[cfg(feature = "serde")]
impl<'a, T> Display for List<T>
where
    T: Clone
        + PartialEq
        + Eq
        + Debug
        + Display
        + serde::Serialize
        + StrictEncode
        + StrictDecode,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(
            &serde_yaml::to_string(self)
                .expect("internal YAML serialization error"),
        )
    }
}

impl<T> FromIterator<T> for List<T>
where
    T: Clone
        + PartialEq
        + Eq
        + Debug
        + Display
        + serde::Serialize
        + StrictEncode
        + StrictDecode,
{
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::from_inner(iter.into_iter().collect())
    }
}

#[cfg(feature = "serde")]
impl<T> serde::Serialize for List<T>
where
    T: Clone
        + PartialEq
        + Eq
        + Debug
        + Display
        + serde::Serialize
        + StrictEncode
        + StrictDecode,
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

#[derive(
    Wrapper,
    Clone,
    PartialEq,
    Eq,
    Debug,
    From,
    Default,
    StrictEncode,
    StrictDecode,
)]
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
