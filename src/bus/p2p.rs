use farcaster_core::{
    protocol::message::Abort,
    swap::btcxmr::message::{
        BuyProcedureSignature, CommitAliceParameters, CommitBobParameters, CoreArbitratingSetup,
        RefundProcedureSignatures, RevealAliceParameters, RevealBobParameters, RevealProof,
    },
    swap::btcxmr::PublicOffer,
    swap::SwapId,
};
use internet2::Api;
use strict_encoding::{StrictDecode, StrictEncode};

#[derive(Clone, Debug, Display, Api, StrictDecode, StrictEncode)]
#[api(encoding = "strict")]
#[display(inner)]
#[non_exhaustive]
pub enum PeerMsg {
    #[api(type = 33701)]
    #[display("{0} maker commit")]
    MakerCommit(Commit),

    #[api(type = 33702)]
    #[display("{0} taker commit")]
    TakerCommit(TakerCommit),

    #[api(type = 33703)]
    #[display("reveal {0}")]
    Reveal(Reveal),

    #[api(type = 33720)]
    #[display("refund procedure signatures")]
    RefundProcedureSignatures(RefundProcedureSignatures),

    #[api(type = 33710)]
    #[display("core arbitrating setup")]
    CoreArbitratingSetup(CoreArbitratingSetup),

    #[api(type = 33730)]
    #[display("buy procedure signature")]
    BuyProcedureSignature(BuyProcedureSignature),

    #[api(type = 18)]
    #[display("ping({0})")]
    Ping(u16),

    #[api(type = 19)]
    #[display("pong(..)")]
    Pong(Vec<u8>),

    #[api(type = 33798)]
    #[display("identity(..)")]
    Identity(internet2::addr::NodeId),

    #[api(type = 33799)]
    #[display("abort(..)")]
    Abort(Abort),

    #[api(type = 33800)]
    #[display("ping_peer()")]
    PingPeer,

    #[api(type = 33801)]
    #[display("error_shutdown()")]
    PeerReceiverRuntimeShutdown,
}

impl PeerMsg {
    pub fn swap_id(&self) -> SwapId {
        match self {
            PeerMsg::MakerCommit(c) => c.swap_id(),
            PeerMsg::TakerCommit(c) => c.swap_id(),
            PeerMsg::Reveal(m) => match m {
                Reveal::AliceParameters(n) => n.swap_id,
                Reveal::BobParameters(n) => n.swap_id,
                Reveal::Proof(n) => n.swap_id,
            },
            PeerMsg::RefundProcedureSignatures(RefundProcedureSignatures { swap_id, .. }) => {
                *swap_id
            }
            PeerMsg::Abort(Abort { swap_id, .. }) => *swap_id,
            PeerMsg::CoreArbitratingSetup(CoreArbitratingSetup { swap_id, .. }) => *swap_id,
            PeerMsg::BuyProcedureSignature(BuyProcedureSignature { swap_id, .. }) => *swap_id,
            PeerMsg::Ping(_)
            | PeerMsg::Pong(_)
            | PeerMsg::PingPeer
            | PeerMsg::PeerReceiverRuntimeShutdown
            | PeerMsg::Identity(_) => {
                unreachable!(
                    "Ping, Pong, PingPeer, PeerdShutdown and Identity do not contain swapid"
                )
            }
        }
    }

    pub fn on_receiver_whitelist(&self) -> bool {
        matches!(
            self,
            PeerMsg::MakerCommit(_)
                | PeerMsg::TakerCommit(_)
                | PeerMsg::Reveal(_)
                | PeerMsg::RefundProcedureSignatures(_)
                | PeerMsg::CoreArbitratingSetup(_)
                | PeerMsg::BuyProcedureSignature(_)
                | PeerMsg::Ping(_)
                | PeerMsg::Pong(_)
        )
    }

    pub fn is_protocol(&self) -> bool {
        matches!(
            self,
            PeerMsg::MakerCommit(_)
                | PeerMsg::TakerCommit(_)
                | PeerMsg::Reveal(_)
                | PeerMsg::RefundProcedureSignatures(_)
                | PeerMsg::CoreArbitratingSetup(_)
                | PeerMsg::BuyProcedureSignature(_)
        )
    }
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
pub enum Reveal {
    #[display("Alice parameters")]
    AliceParameters(RevealAliceParameters),
    #[display("Bob parameters")]
    BobParameters(RevealBobParameters),
    #[display("proof")]
    Proof(RevealProof),
}

#[derive(Clone, Debug, Display, From, StrictDecode, StrictEncode)]
#[display("{commit}")]
pub struct TakerCommit {
    pub commit: Commit,
    pub public_offer: PublicOffer, // TODO: replace by public offer id
}

impl TakerCommit {
    pub fn swap_id(&self) -> SwapId {
        self.commit.swap_id()
    }
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
pub enum Commit {
    #[display("Alice")]
    AliceParameters(CommitAliceParameters),
    #[display("Bob")]
    BobParameters(CommitBobParameters),
}

impl Commit {
    pub fn swap_id(&self) -> SwapId {
        match self {
            Self::AliceParameters(c) => c.swap_id,
            Self::BobParameters(c) => c.swap_id,
        }
    }
}
