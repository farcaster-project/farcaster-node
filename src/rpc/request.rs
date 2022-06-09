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

#![allow(clippy::clone_on_copy)]

use crate::swapd::CheckpointSwapd;
use crate::walletd::runtime::BobState;
use crate::walletd::{runtime::AliceState, NodeSecrets};
use crate::{
    farcasterd,
    syncerd::{
        types::{Event, Task},
        Coin, SweepXmrAddress,
    },
};
use amplify::{Holder, ToYamlString, Wrapper};
use farcaster_core::syncer::BroadcastTransaction;
use farcaster_core::{bundle::SignedArbitratingLock, syncer::Abort};
use farcaster_core::{
    consensus::{Decodable as CoreDecodable, Encodable as CoreEncodable},
    negotiation::PublicOfferId,
};
use internet2::addr::InetSocketAddr;
use internet2::{CreateUnmarshaller, Payload, Unmarshall, Unmarshaller};
use lazy_static::lazy_static;
use lightning_encoding::{strategies::AsStrict, LightningDecode, LightningEncode};
use monero::consensus::{Decodable as MoneroDecodable, Encodable as MoneroEncodable};
#[cfg(feature = "serde")]
use serde_with::{skip_serializing_none, DisplayFromStr, DurationSeconds, Same};
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
    Address, OutPoint, PublicKey, Transaction,
};
use farcaster_core::{
    bitcoin::BitcoinSegwitV0,
    blockchain::FeePriority,
    bundle::{
        AliceParameters, BobParameters, CoreArbitratingTransactions, CosignedArbitratingCancel,
        Proof,
    },
    monero::Monero,
    negotiation::{Offer, PublicOffer},
    protocol_message::{
        self, CommitAliceParameters, CommitBobParameters, RevealAliceParameters,
        RevealBobParameters, RevealProof,
    },
    role::TradeRole,
    swap::btcxmr::BtcXmr,
    swap::{Swap, SwapId},
};
use internet2::Api;
use internet2::{NodeAddr, RemoteSocketAddr};
use lnp::p2p::legacy::{Messages, Ping, TempChannelId as TempSwapId};
use microservices::rpc::Failure;
use microservices::rpc_connection;
use strict_encoding::{
    strategies::HashFixedBytes, strict_encode_list, Strategy, StrictDecode, StrictEncode,
};

#[derive(Clone, Debug, Display, From, StrictDecode, StrictEncode, Api)]
#[api(encoding = "lightning")]
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
    Ping(Ping),
    #[api(type = 31)]
    #[display("pong(...)")]
    Pong(Vec<u8>),
    #[api(type = 33)]
    #[display("ping_peer")]
    PingPeer,
    #[api(type = 34)]
    #[display("error_shutdown")]
    PeerReceiverRuntimeShutdown,
    #[api(type = 35)]
    #[display("identity")]
    Identity(bitcoin::secp256k1::PublicKey),
}

impl Msg {
    pub fn swap_id(&self) -> SwapId {
        match self {
            Msg::MakerCommit(m) => match m {
                Commit::AliceParameters(n) => n.swap_id,
                Commit::BobParameters(n) => n.swap_id,
            },
            Msg::TakerCommit(TakeCommit { swap_id, .. }) => *swap_id,
            Msg::Reveal(m) => match m {
                Reveal::AliceParameters(n) => n.swap_id,
                Reveal::BobParameters(n) => n.swap_id,
                Reveal::Proof(n) => n.swap_id,
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
            Msg::Ping(_)
            | Msg::Pong(_)
            | Msg::PingPeer
            | Msg::PeerReceiverRuntimeShutdown
            | Msg::Identity(_) => {
                unreachable!(
                    "Ping, Pong, PingPeer, PeerdShutdown and Identity do not contain swapid"
                )
            }
        }
    }

    pub fn on_receiver_whitelist(&self) -> bool {
        matches!(
            self,
            Msg::MakerCommit(_)
                | Msg::TakerCommit(_)
                | Msg::Reveal(_)
                | Msg::RefundProcedureSignatures(_)
                | Msg::CoreArbitratingSetup(_)
                | Msg::BuyProcedureSignature(_)
                | Msg::Ping(_)
                | Msg::Pong(_)
        )
    }
}

impl LightningEncode for Msg {
    fn lightning_encode<E: io::Write>(&self, e: E) -> Result<usize, lightning_encoding::Error> {
        Payload::from(self.clone()).lightning_encode(e)
    }
}

lazy_static! {
    pub static ref UNMARSHALLER: Unmarshaller<Msg> = Msg::create_unmarshaller();
}

impl LightningDecode for Msg {
    fn lightning_decode<D: io::Read>(d: D) -> Result<Self, lightning_encoding::Error> {
        Ok((&*UNMARSHALLER
            .unmarshall(&*Vec::<u8>::lightning_decode(d)?)
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
#[display("commit")]
pub enum Commit {
    AliceParameters(CommitAliceParameters<BtcXmr>),
    BobParameters(CommitBobParameters<BtcXmr>),
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("nodeid")]
pub struct NodeId(pub bitcoin::secp256k1::PublicKey);

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("{public_offer}")]
pub struct PubOffer {
    pub public_offer: PublicOffer<BtcXmr>,
    pub peer_secret_key: Option<SecretKey>,
    pub external_address: Address,
    pub internal_address: String,
}

impl From<(PublicOffer<BtcXmr>, Address, String)> for PubOffer {
    fn from(x: (PublicOffer<BtcXmr>, Address, String)) -> Self {
        let (public_offer, external_address, internal_address) = x;
        PubOffer {
            public_offer,
            external_address,
            internal_address,
            peer_secret_key: None,
        }
    }
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, PartialEq, Eq)]
#[display("{0}")]
pub struct Token(pub String);

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("get keys(token({0}), req_id({1}))")]
pub struct GetKeys(pub Token, pub RequestId);

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("reconnect peer(node addr({0})))")]
pub struct ReconnectPeer(pub NodeAddr, pub Option<SecretKey>);

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("launch_swap")]
pub struct LaunchSwap {
    pub local_trade_role: TradeRole,
    pub public_offer: PublicOffer<BtcXmr>,
    pub local_params: Params,
    pub swap_id: SwapId,
    pub remote_commit: Option<Commit>,
    pub funding_address: Option<bitcoin::Address>,
}

#[derive(Clone, Debug, From, StrictDecode, StrictEncode)]
pub struct TakeCommit {
    pub commit: Commit,
    pub public_offer: String, // TODO: replace by public offer id
    pub swap_id: SwapId,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq)]
#[display(format_keys)]
pub struct Keys(
    pub bitcoin::secp256k1::SecretKey,
    pub bitcoin::secp256k1::PublicKey,
    pub RequestId,
);

fn format_keys(keys: &Keys) -> String {
    format!(
        "sk: {}, pk: {}, id: {}",
        keys.0.display_secret(),
        keys.1,
        keys.2
    )
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("reveal")]
pub enum Reveal {
    AliceParameters(RevealAliceParameters<BtcXmr>),
    BobParameters(RevealBobParameters<BtcXmr>),
    Proof(RevealProof<BtcXmr>),
}

// #[cfg_attr(feature = "serde", serde_as)]
// #[cfg_attr(
//     feature = "serde",
//     derive(Serialize, Deserialize),
//     serde(crate = "serde_crate")
// )]
#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("params")]
pub enum Params {
    Alice(AliceParameters<BtcXmr>),
    Bob(BobParameters<BtcXmr>),
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display(inner)]
pub enum Tx {
    #[display("lock")]
    Lock(Transaction),
    #[display("buy")]
    Buy(Transaction),
    #[display("funding")]
    Funding(Transaction),
    #[display("cancel")]
    Cancel(Transaction),
    #[display("refund")]
    Refund(Transaction),
    #[display("punish")]
    Punish(Transaction),
}

use crate::{Error, ServiceId};

#[derive(Clone, Debug, Display, From, Api)]
#[api(encoding = "strict")]
#[non_exhaustive]
pub enum Request {
    // Part I: Generic messages outside of channel operations
    // ======================================================
    /// Once authentication is complete, the first message reveals the features
    /// supported or required by this node, even if this is a reconnection.
    // #[api(type = 16)]
    // #[display(inner)]
    // Init(message::Init),

    /// For simplicity of diagnosis, it's often useful to tell a peer that
    /// something is incorrect.
    // #[api(type = 17)]
    // #[display(inner)]
    // Error(message::Error),

    /// In order to allow for the existence of long-lived TCP connections, at
    /// times it may be required that both ends keep alive the TCP connection
    /// at the application level. Such messages also allow obfuscation of
    /// traffic patterns.
    // #[api(type = 18)]
    // #[display(inner)]
    // Ping(message::Ping),

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

    #[api(type = 2)]
    #[display("send_message({0})")]
    PeerMessage(Messages),

    #[api(type = 3)]
    #[display("terminate")]
    Terminate,

    #[api(type = 4)]
    #[display("peerd_terminated")]
    PeerdTerminated,

    #[api(type = 5)]
    #[display("send_message({0})")]
    Protocol(Msg),

    #[api(type = 6)]
    #[display("peerd_unreachable")]
    PeerdUnreachable(ServiceId),

    #[api(type = 7)]
    #[display("reconnect_peer")]
    ReconnectPeer(ReconnectPeer),

    #[api(type = 8)]
    #[display("peerd_reconnected")]
    PeerdReconnected,

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

    #[api(type = 46)]
    #[display("swap_success()")]
    SwapOutcome(Outcome),

    // Can be issued from `cli` to `lnpd`
    #[api(type = 100)]
    #[display(Debug)]
    GetInfo(Option<u64>),

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

    // TODO: only list offers matching list of OfferIds
    #[api(type = 104)]
    #[display("list_offers()")]
    ListOffers,

    // #[api(type = 105)]
    // #[display("list_offer_ids()")]
    // ListOfferIds,
    #[api(type = 105)]
    #[display("list_listens()")]
    ListListens,

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

    #[api(type = 206)]
    #[display("made_offer({0})", alt = "{0:#}")]
    MadeOffer(MadeOffer),

    #[api(type = 207)]
    #[display("took_offer({0})", alt = "{0:#}")]
    TookOffer(TookOffer),

    #[api(type = 197)]
    #[display("params({0:#})")]
    Params(Params),

    #[api(type = 196)]
    #[display("transaction({0})")]
    Tx(Tx),

    #[api(type = 195)]
    #[display("bitcoin address()")]
    BitcoinAddress(BitcoinAddress),

    #[api(type = 194)]
    #[display("monero address()")]
    MoneroAddress(MoneroAddress),

    #[api(type = 193)]
    #[display("revoke_offer({0})")]
    RevokeOffer(PublicOffer<BtcXmr>),

    #[api(type = 205)]
    #[display("fund_swap({0})")]
    FundSwap(OutPoint),

    // Responses to CLI
    // ----------------
    #[api(type = 1004)]
    #[display("{0}")]
    String(String),

    #[api(type = 1002)]
    #[display("progress: {0}")]
    Progress(Progress),

    #[api(type = 1005)]
    #[display(inner)]
    SwapProgress(SwapProgress),

    #[api(type = 1003)]
    #[display("read_progress({0})")]
    ReadProgress(SwapId),

    #[api(type = 1006)]
    #[display("subscribe_progress({0})")]
    SubscribeProgress(SwapId),

    #[api(type = 1007)]
    #[display("unsubscribe_progress({0})")]
    UnsubscribeProgress(SwapId),

    #[api(type = 1001)]
    #[display(inner)]
    Success(OptionDetails),

    #[api(type = 1000)]
    #[display(inner)]
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

    // TODO: only list offers matching list of OfferIds
    #[api(type = 1106)]
    #[display("offer_list({0})", alt = "{0:#}")]
    #[from]
    OfferList(List<PublicOffer<BtcXmr>>),

    // #[api(type = 1107)]
    // #[display("offer_list({0})", alt = "{0:#}")]
    // #[from]
    // OfferIdList(List<PublicOfferId>),
    #[api(type = 1107)]
    #[display("offer_list({0})", alt = "{0:#}")]
    #[from]
    ListenList(List<String>),

    #[api(type = 1108)]
    #[display("funding_info({0})", alt = "{0:#}")]
    #[from]
    FundingInfo(FundingInfo),

    #[api(type = 1109)]
    #[display("needs_funding({0})")]
    NeedsFunding(Coin),

    #[api(type = 1110)]
    #[display("write_text")]
    WriteText(List<String>),

    #[api(type = 1111)]
    #[display("funding_completed({0})")]
    FundingCompleted(Coin),

    #[api(type = 1112)]
    #[display("read_funding")]
    FundingCanceled(Coin),

    // #[api(type = 1203)]
    // #[display("channel_funding({0})", alt = "{0:#}")]
    // #[from]
    // SwapFunding(PubkeyScript),
    // #[api(type = 1300)]
    // #[display("task({0})", alt = "{0:#}")]
    // #[from]
    // CreateTask(u64), // FIXME
    #[api(type = 1300)]
    #[display("syncer_task({0})", alt = "{0:#}")]
    #[from]
    SyncerTask(Task),

    #[api(type = 1301)]
    #[display("syncer_event({0})", alt = "{0:#}")]
    #[from]
    SyncerEvent(Event),

    #[api(type = 1302)]
    #[display("syncer_bridge_ev({0})", alt = "{0:#}")]
    #[from]
    SyncerdBridgeEvent(SyncerdBridgeEvent),

    #[api(type = 1303)]
    #[display("task({0})", alt = "{0:#}")]
    #[from]
    SweepXmrAddress(SweepXmrAddress),

    #[api(type = 1304)]
    #[display("checkpoint({0})", alt = "{0:#}")]
    #[from]
    Checkpoint(Checkpoint),

    #[api(type = 1305)]
    #[display("checkpoint_multipart({0})")]
    #[from]
    CheckpointMultipartChunk(CheckpointMultipartChunk),

    #[api(type = 1306)]
    #[display("retrieve_all_checkpoint_info")]
    RetrieveAllCheckpointInfo,

    #[api(type = 1307)]
    #[display("delete_checkpoint")]
    RemoveCheckpoint(SwapId),

    #[api(type = 1308)]
    #[display("checkpoint_list({0})", alt = "{0:#}")]
    CheckpointList(List<CheckpointEntry>),

    #[api(type = 1309)]
    #[display("restore_checkpoint({0})", alt = "{0:#}")]
    RestoreCheckpoint(SwapId),
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
pub enum Outcome {
    #[display("Success(Swapped)")]
    Buy,
    #[display("Failure(Refunded)")]
    Refund,
    #[display("Failure(Punished)")]
    Punish,
}
#[derive(Clone, Debug, Display, StrictDecode, StrictEncode)]
#[display("funding_info")]
pub enum FundingInfo {
    Bitcoin(BitcoinFundingInfo),
    Monero(MoneroFundingInfo),
}

#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
#[display("{swap_id}, {public_offer}")]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(NodeInfo::to_yaml_string)]
pub struct CheckpointEntry {
    pub swap_id: SwapId,
    pub public_offer: PublicOffer<BtcXmr>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct CheckpointChunk {
    pub msg_index: usize,
    pub serialized_state_chunk: Vec<u8>,
}

#[derive(Clone, Debug, Display, StrictDecode, StrictEncode)]
#[display("{msg_index}, {msgs_total}, {swap_id}")]
pub struct CheckpointMultipartChunk {
    pub checksum: [u8; 20],
    pub msg_index: usize,
    pub msgs_total: usize,
    pub serialized_state_chunk: Vec<u8>,
    pub swap_id: SwapId,
}

#[derive(Clone, Debug, Display, StrictDecode, StrictEncode)]
#[display(Debug)]
pub struct Checkpoint {
    pub swap_id: SwapId,
    pub state: CheckpointState,
}

#[derive(Clone, Debug, Display, StrictDecode, StrictEncode)]
#[display(Debug)]
pub enum CheckpointState {
    CheckpointWalletAlice(AliceState),
    CheckpointWalletBob(BobState),
    CheckpointSwapd(CheckpointSwapd),
}

impl FromStr for BitcoinFundingInfo {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        let content: Vec<&str> = s.split(" ").collect();

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

#[derive(Clone, Debug, StrictDecode, StrictEncode)]
pub struct BitcoinFundingInfo {
    pub swap_id: SwapId,
    pub address: bitcoin::Address,
    pub amount: bitcoin::Amount,
}

impl StrictEncode for MoneroFundingInfo {
    fn strict_encode<E: ::std::io::Write>(
        &self,
        mut e: E,
    ) -> Result<usize, strict_encoding::Error> {
        let mut len = self.swap_id.strict_encode(&mut e)?;
        len += self
            .amount
            .as_pico()
            .strict_encode(&mut e)
            .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?;

        use monero::consensus::Encodable;

        Ok(len
            + self
                .address
                .consensus_encode(&mut e)
                .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?)
    }
}

impl StrictDecode for MoneroFundingInfo {
    fn strict_decode<D: ::std::io::Read>(mut d: D) -> Result<Self, strict_encoding::Error> {
        Ok(Self {
            swap_id: SwapId::strict_decode(&mut d)?,
            amount: monero::Amount::from_pico(
                u64::strict_decode(&mut d)
                    .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?,
            ),
            address: monero::Address::consensus_decode(&mut d)
                .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?,
        })
    }
}

impl FromStr for MoneroFundingInfo {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        let content: Vec<&str> = s.split(" ").collect();
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

#[derive(Clone, Debug)]
pub struct MoneroFundingInfo {
    pub swap_id: SwapId,
    pub amount: monero::Amount,
    pub address: monero::Address,
}

impl rpc_connection::Request for Request {}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("{source}, {event}")]
pub struct SyncerdBridgeEvent {
    pub event: Event,
    pub source: ServiceId,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("{peerd}, {swap_id}")]
pub struct InitSwap {
    pub peerd: ServiceId,
    pub report_to: Option<ServiceId>,
    pub local_params: Params,
    pub swap_id: SwapId,
    pub remote_commit: Option<Commit>,
    pub funding_address: Option<bitcoin::Address>,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display(inner)]
pub enum Progress {
    Message(String),
    StateTransition(String),
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
    #[serde(skip_serializing)]
    pub id: Option<u64>,
    pub node_ids: Vec<secp256k1::PublicKey>,
    pub listens: Vec<RemoteSocketAddr>,
    #[serde_as(as = "DurationSeconds")]
    pub uptime: Duration,
    pub since: u64,
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub peers: Vec<NodeAddr>,
    pub swaps: Vec<SwapId>,
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub offers: Vec<PublicOffer<BtcXmr>>,
}

#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
#[display("bitcoin_address")]
pub struct BitcoinAddress(pub SwapId, pub bitcoin::Address);

impl StrictEncode for MoneroAddress {
    fn strict_encode<E: ::std::io::Write>(
        &self,
        mut e: E,
    ) -> Result<usize, strict_encoding::Error> {
        let len = self
            .0
            .consensus_encode(&mut e)
            .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?;
        use monero::consensus::Encodable;
        Ok(len
            + self
                .1
                .consensus_encode(&mut e)
                .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?)
    }
}

impl StrictDecode for MoneroAddress {
    fn strict_decode<D: ::std::io::Read>(mut d: D) -> Result<Self, strict_encoding::Error> {
        Ok(Self(
            SwapId::consensus_decode(&mut d)
                .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?,
            monero::Address::consensus_decode(&mut d)
                .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?,
        ))
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Display)]
#[display("monero_address")]
pub struct MoneroAddress(pub SwapId, pub monero::Address);

#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
#[display("proto_puboffer")]
pub struct ProtoPublicOffer {
    pub offer: Offer<BtcXmr>,
    pub public_addr: RemoteSocketAddr,
    pub bind_addr: RemoteSocketAddr,
    pub arbitrating_addr: bitcoin::Address,
    pub accordant_addr: String,
    pub peer_secret_key: Option<SecretKey>,
    pub peer_public_key: Option<secp256k1::PublicKey>,
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
    pub offer: String,
    pub message: String,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(TookOffer::to_yaml_string)]
pub struct TookOffer {
    pub offerid: PublicOfferId,
    pub message: String,
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
    pub forked_from_listener: bool,
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
    pub local_keys: lnp::channel::bolt::LocalKeyset,
    #[serde_as(as = "BTreeMap<DisplayFromStr, Same>")]
    pub remote_keys: BTreeMap<NodeAddr, lnp::channel::bolt::RemoteKeyset>,
}

#[cfg(feature = "serde")]
impl ToYamlString for NodeInfo {}
#[cfg(feature = "serde")]
impl ToYamlString for PeerInfo {}
#[cfg(feature = "serde")]
impl ToYamlString for SwapInfo {}
#[cfg(feature = "serde")]
impl ToYamlString for SyncerInfo {}
#[cfg(feature = "serde")]
impl ToYamlString for MadeOffer {}
#[cfg(feature = "serde")]
impl ToYamlString for TookOffer {}
#[cfg(feature = "serde")]
impl ToYamlString for SwapProgress {}
#[cfg(feature = "serde")]
impl ToYamlString for ProgressEvent {}
#[cfg(feature = "serde")]
impl ToYamlString for CheckpointEntry {}

#[derive(Wrapper, Clone, PartialEq, Eq, Debug, From, StrictEncode, StrictDecode)]
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

impl From<crate::Error> for Request {
    fn from(err: crate::Error) -> Self {
        Request::Failure(Failure::from(err))
    }
}

pub trait IntoProgressOrFailure {
    fn into_progress_or_failure(self) -> Request;
}
pub trait IntoSuccessOrFailure {
    fn into_success_or_failure(self) -> Request;
}

impl IntoProgressOrFailure for Result<String, crate::Error> {
    fn into_progress_or_failure(self) -> Request {
        match self {
            Ok(val) => Request::Progress(Progress::Message(val)),
            Err(err) => Request::from(err),
        }
    }
}

impl IntoSuccessOrFailure for Result<String, crate::Error> {
    fn into_success_or_failure(self) -> Request {
        match self {
            Ok(val) => Request::Success(OptionDetails::with(val)),
            Err(err) => Request::from(err),
        }
    }
}

impl IntoSuccessOrFailure for Result<(), crate::Error> {
    fn into_success_or_failure(self) -> Request {
        match self {
            Ok(_) => Request::Success(OptionDetails::new()),
            Err(err) => Request::from(err),
        }
    }
}

impl From<(SwapId, Params)> for Reveal {
    fn from(tuple: (SwapId, Params)) -> Self {
        match tuple {
            (swap_id, Params::Alice(params)) => Reveal::AliceParameters((swap_id, params).into()),
            (swap_id, Params::Bob(params)) => Reveal::BobParameters((swap_id, params).into()),
        }
    }
}

impl From<(SwapId, Proof<BtcXmr>)> for Reveal {
    fn from(tuple: (SwapId, Proof<BtcXmr>)) -> Self {
        Reveal::Proof((tuple.0, tuple.1).into())
    }
}
