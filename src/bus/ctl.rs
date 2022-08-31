use crate::{ServiceId};
use crate::bus::msg::{Commit};

use farcaster_core::{
    role::TradeRole,
    swap::btcxmr::{Parameters, Offer, PublicOffer},
    swap::SwapId,
};

use bitcoin::{ secp256k1::{ SecretKey,}};
use internet2::addr::{InetSocketAddr, NodeAddr};
use internet2::Api;
use strict_encoding::{StrictDecode, StrictEncode};

#[derive(Clone, Debug, Display, From, StrictDecode, StrictEncode, Api)]
#[api(encoding = "strict")]
#[non_exhaustive]
pub enum Ctl {
    #[api(type = 0)]
    #[display("hello()")]
    Hello,

    #[api(type = 3)]
    #[display("terminate()")]
    Terminate,

    #[api(type = 204)]
    #[display("make_swap({0})")]
    MakeSwap(InitSwap),

    #[api(type = 203)]
    #[display("take_swap({0})")]
    TakeSwap(InitSwap),

    #[api(type = 29)]
    #[display("launch_swap({0})")]
    LaunchSwap(LaunchSwap),

    #[api(type = 197)]
    #[display("params({0})")]
    Params(Params),

    //#[api(type = 7)]
    //#[display("reconnect_peer({0})")]
    //ReconnectPeer(ReconnectPeer),

    //#[api(type = 32)]
    //#[display("node_id({0})")]
    //NodeId(NodeId),

    #[api(type = 6)]
    #[display("peerd_unreachable({0})")]
    PeerdUnreachable(ServiceId),

    #[api(type = 8)]
    #[display("peerd_reconnected({0})")]
    PeerdReconnected(ServiceId),

    #[api(type = 4)]
    #[display("peerd_terminated()")]
    PeerdTerminated,

    #[api(type = 1309)]
    #[display("restore_checkpoint({0})", alt = "{0:#}")]
    RestoreCheckpoint(SwapId),

    #[api(type = 198)]
    #[display("make_offer({0})")]
    MakeOffer(ProtoPublicOffer),

    #[api(type = 199)]
    #[display("take_offer({0}))")]
    TakeOffer(PubOffer),

    #[api(type = 30)]
    #[display("get_keys({0})")]
    GetKeys(GetKeys),

    #[api(type = 193)]
    #[display("revoke_offer({0})")]
    RevokeOffer(PublicOffer),

    #[api(type = 192)]
    #[display("abort_swap()")]
    AbortSwap,
}

#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
#[display("..")]
pub struct ProtoPublicOffer {
    pub offer: Offer,
    pub public_addr: InetSocketAddr,
    pub bind_addr: InetSocketAddr,
    pub arbitrating_addr: bitcoin::Address,
    pub accordant_addr: monero::Address,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("{public_offer}, ..")]
pub struct PubOffer {
    pub public_offer: PublicOffer,
    pub external_address: bitcoin::Address,
    pub internal_address: monero::Address,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("{0}, ..")]
pub struct ReconnectPeer(pub NodeAddr, pub Option<SecretKey>);

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, PartialEq, Eq)]
#[display("{0}")]
pub struct Token(pub String);

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("token({0})")]
pub struct GetKeys(pub Token);

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("{public_offer}, ..")]
pub struct LaunchSwap {
    pub local_trade_role: TradeRole,
    pub public_offer: PublicOffer,
    pub local_params: Params,
    pub swap_id: SwapId,
    pub remote_commit: Option<Commit>,
    pub funding_address: Option<bitcoin::Address>,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
pub enum Params {
    #[display("alice(..)")]
    Alice(Parameters),
    #[display("bob(..)")]
    Bob(Parameters),
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("{peerd}, {swap_id}, ..")]
pub struct InitSwap {
    pub peerd: ServiceId,
    pub report_to: Option<ServiceId>,
    pub local_params: Params,
    pub swap_id: SwapId,
    pub remote_commit: Option<Commit>,
    pub funding_address: Option<bitcoin::Address>,
}
