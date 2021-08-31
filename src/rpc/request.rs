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
use amplify::{Holder, ToYamlString, Wrapper};
use farcaster_core::{bundle::SignedArbitratingLock, syncer::Abort};
use farcaster_core::syncer::BroadcastTransaction;
use farcaster_core::syncer::Event;
use farcaster_core::syncer::HeightChanged;
use farcaster_core::syncer::Task;
use farcaster_core::syncer::WatchAddress;
use farcaster_core::syncer::WatchHeight;
use farcaster_core::syncer::WatchTransaction;
use internet2::addr::InetSocketAddr;
use internet2::{CreateUnmarshaller, Payload, Unmarshall, Unmarshaller};
use lazy_static::lazy_static;
use lightning_encoding::{strategies::AsStrict, LightningDecode, LightningEncode};
#[cfg(feature = "serde")]
use serde_with::{DisplayFromStr, DurationSeconds, Same};
use std::{collections::BTreeMap, convert::TryInto};
use std::{
    fmt::{self, Debug, Display, Formatter},
    io,
    marker::PhantomData,
};
use std::{iter::FromIterator, str::FromStr};
use std::{num::ParseIntError, time::Duration};

use bitcoin::{
    secp256k1::{
        self,
        rand::{thread_rng, RngCore},
        SecretKey,
    },
    OutPoint, PublicKey,
};
use farcaster_core::{
    bitcoin::BitcoinSegwitV0,
    blockchain::FeePriority,
    bundle::{
        AliceParameters, BobParameters, CoreArbitratingTransactions, CosignedArbitratingCancel,
    },
    monero::Monero,
    negotiation::{Offer, PublicOffer},
    protocol_message::{self, RevealAliceParameters, RevealBobParameters},
    protocol_message::{CommitAliceParameters, CommitBobParameters},
    role::TradeRole,
    swap::btcxmr::BtcXmr,
    swap::SwapId,
};
use internet2::Api;
use internet2::{NodeAddr, RemoteSocketAddr};
use lnp::payment::{self, AssetsBalance, Lifecycle};
use lnp::{message, Messages, TempChannelId as TempSwapId};
use lnpbp::strict_encoding::{
    strategies::HashFixedBytes, strict_encode_list, Strategy, StrictDecode, StrictEncode,
};
use microservices::rpc::Failure;
use microservices::rpc_connection;

#[derive(Clone, Debug, Display, From, StrictDecode, StrictEncode, Api)]
#[api(encoding = "lightning")]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display(inner)]
pub enum Msg {
    #[api(type = 28)]
    #[display("maker_commit(...)")]
    MakerCommit(Commit),
    #[api(type = 21)]
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
    #[api(type = 29)]
    #[display(inner)]
    Ping(message::Ping),
    #[api(type = 31)]
    #[display("pong(...)")]
    Pong(Vec<u8>),
    #[api(type = 33)]
    #[display("ping_peer")]
    PingPeer,
}

impl Msg {
    pub fn swap_id(&self) -> SwapId {
        match self {
            Msg::MakerCommit(m) => match m {
                Commit::Alice(n) => n.swap_id,
                Commit::Bob(n) => n.swap_id,
            },
            Msg::TakerCommit(TakeCommit { swap_id, .. }) => *swap_id,
            Msg::Reveal(m) => match m {
                Reveal::Alice(n) => n.swap_id,
                Reveal::Bob(n) => n.swap_id,
            },
            Msg::RefundProcedureSignatures(protocol_message::RefundProcedureSignatures {
                swap_id,
                ..
            }) => *swap_id,
            Msg::Abort(protocol_message::Abort { swap_id, .. }) => *swap_id,
            Msg::CoreArbitratingSetup(protocol_message::CoreArbitratingSetup {
                swap_id, ..
            }) => *swap_id,
            Msg::BuyProcedureSignature(protocol_message::BuyProcedureSignature {
                swap_id, ..
            }) => *swap_id,
            Msg::Ping(_) | Msg::Pong(_) | Msg::PingPeer => {
                unreachable!("Ping and Pong does not containt swapid")
            }
        }
    }
}

impl LightningEncode for Msg {
    fn lightning_encode<E: io::Write>(&self, e: E) -> Result<usize, io::Error> {
        Payload::from(self.clone()).lightning_encode(e)
    }
}

lazy_static! {
    pub static ref UNMARSHALLER: Unmarshaller<Msg> = Msg::create_unmarshaller();
}

impl LightningDecode for Msg {
    fn lightning_decode<D: io::Read>(d: D) -> Result<Self, lightning_encoding::Error> {
        Ok((&*UNMARSHALLER
            .unmarshall(&Vec::<u8>::lightning_decode(d)?)
            .map_err(|_| {
                lightning_encoding::Error::DataIntegrityError(s!("can't unmarshall FWP message"))
            })?)
            .clone())
    }
}

impl lightning_encoding::Strategy for Commit {
    type Strategy = AsStrict;
}
impl lightning_encoding::Strategy for TakeCommit {
    type Strategy = AsStrict;
}
impl lightning_encoding::Strategy for Reveal {
    type Strategy = AsStrict;
}

#[derive(Eq, PartialEq, Hash, Clone, Debug, Display, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("request_id({0})")]
pub struct RequestId(pub u64);

impl RequestId {
    pub fn rand() -> RequestId {
        let mut id = [0u8; 8];
        thread_rng().fill_bytes(&mut id);
        RequestId(u64::from_be_bytes(id))
    }
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
#[display("{public_offer}")]
pub struct PubOffer {
    pub public_offer: PublicOffer<BtcXmr>,
    pub peer_secret_key: Option<SecretKey>,
}

impl From<PublicOffer<BtcXmr>> for PubOffer {
    fn from(public_offer: PublicOffer<BtcXmr>) -> Self {
        PubOffer {
            public_offer,
            peer_secret_key: None,
        }
    }
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, PartialEq, Eq)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("{0}")]
pub struct Token(pub String);

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("get keys(token({0}), req_id({1}))")]
pub struct GetKeys(pub Token, pub RequestId);

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("launch_swap")]
pub struct LaunchSwap {
    pub peer: ServiceId,
    pub local_trade_role: TradeRole,
    pub public_offer: PublicOffer<BtcXmr>,
    pub local_params: Params,
    pub swap_id: SwapId,
    pub remote_commit: Option<Commit>,
    pub funding_address: Option<bitcoin::Address>,
}

#[derive(Clone, Debug, From, StrictDecode, StrictEncode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
pub struct TakeCommit {
    pub commit: Commit,
    pub public_offer_hex: String, // TODO: replace by public offer id
    pub swap_id: SwapId,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("keypair(sk:({0}),pk:({1})),id:({2})")]
pub struct Keys(
    pub bitcoin::secp256k1::SecretKey,
    pub bitcoin::secp256k1::PublicKey,
    pub RequestId,
);

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

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("datum")]
pub enum Datum {
    SignedArbitratingLock((
        SignedArbitratingLock<BitcoinSegwitV0>,
        bitcoin::secp256k1::PublicKey,
    )),
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

    #[api(type = 32)]
    #[display("nodeid({0})")]
    NodeId(NodeId),

    #[api(type = 30)]
    #[display("get_keys({0})")]
    GetKeys(GetKeys),

    #[api(type = 29)]
    #[display("launch_swap({0})")]
    LaunchSwap(LaunchSwap),

    #[api(type = 28)]
    #[display("keys({0})")]
    Keys(Keys),

    #[api(type = 45)]
    #[display("funding_updated()")]
    FundingUpdated,

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
    #[display("take_swap({0})")]
    TakeSwap(InitSwap),

    // Can be issued from `cli` to `lnpd`
    #[api(type = 204)]
    #[display("make_swap({0})")]
    MakeSwap(InitSwap),

    #[api(type = 199)]
    #[display("public_offer({0:#}))")]
    TakeOffer(PubOffer),

    #[api(type = 198)]
    #[display("proto_puboffer({0:#})")]
    MakeOffer(ProtoPublicOffer),

    #[api(type = 197)]
    #[display("params({0:#})")]
    Params(Params),

    #[api(type = 196)]
    #[display("datum({0})")]
    Datum(Datum),

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
    // #[api(type = 1300)]
    // #[display("task({0})", alt = "{0:#}")]
    // #[from]
    // CreateTask(u64), // FIXME
    #[api(type = 1300)]
    #[display("task({0})", alt = "{0:#}")]
    #[from]
    SyncerTask(Task),

    #[api(type = 1301)]
    #[display("task({0})", alt = "{0:#}")]
    #[from]
    SyncerEvent(Event),

    #[api(type = 1302)]
    #[display("task({0})", alt = "{0:#}")]
    #[from]
    SyncerdBridgeEvent(SyncerdBridgeEvent),
}

impl rpc_connection::Request for Request {}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("syncerd event")]
pub struct SyncerdBridgeEvent {
    pub event: Event,
    pub source: ServiceId,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[strict_encoding_crate(lnpbp::strict_encoding)]
#[display("{peerd}, {swap_id}")]
pub struct InitSwap {
    pub peerd: ServiceId,
    pub report_to: Option<ServiceId>,
    pub local_params: Params,
    pub swap_id: SwapId,
    pub remote_commit: Option<Commit>,
    pub funding_address: Option<bitcoin::Address>,
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
    pub node_ids: Vec<secp256k1::PublicKey>,
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
    pub peer_secret_key: Option<SecretKey>,
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
    // #[serde_as(as = "BTreeMap<DisplayFromStr, Same>")]
    // pub funding_outpoint: OutPoint,
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub maker_peer: Vec<NodeAddr>,
    #[serde_as(as = "DurationSeconds")]
    pub uptime: Duration,
    pub since: u64,
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

impl Into<Reveal> for (SwapId, Params) {
    fn into(self) -> Reveal {
        match self {
            (swap_id, Params::Alice(params)) => Reveal::Alice((swap_id, params).into()),
            (swap_id, Params::Bob(params)) => Reveal::Bob((swap_id, params).into()),
        }
    }
}
