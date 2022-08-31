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

#[derive(Clone, Debug, Display, From, StrictDecode, StrictEncode, Api)]
#[api(encoding = "strict")]
#[display(inner)]
pub enum Msg {
    #[api(type = 28)]
    #[display("maker_commit({0})")]
    MakerCommit(Commit),

    #[api(type = 21)]
    #[display("taker_commit({0})")]
    TakerCommit(TakeCommit),

    #[api(type = 22)]
    #[display("reveal({0})")]
    Reveal(Reveal),

    #[api(type = 25)]
    #[display("refund_procedure_signatures(..)")]
    RefundProcedureSignatures(RefundProcedureSignatures),

    #[api(type = 27)]
    #[display("abort(..)")]
    Abort(Abort),

    #[api(type = 24)]
    #[display("core_arbitrating_setup(..)")]
    CoreArbitratingSetup(CoreArbitratingSetup),

    #[api(type = 26)]
    #[display("buy_procedure_signature(..)")]
    BuyProcedureSignature(BuyProcedureSignature),

    #[api(type = 29)]
    #[display("ping({0})")]
    Ping(u16),

    #[api(type = 31)]
    #[display("pong(..)")]
    Pong(Vec<u8>),

    #[api(type = 33)]
    #[display("ping_peer()")]
    PingPeer,

    #[api(type = 34)]
    #[display("error_shutdown()")]
    PeerReceiverRuntimeShutdown,

    #[api(type = 35)]
    #[display("identity(..)")]
    Identity(internet2::addr::NodeId),
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
            Msg::RefundProcedureSignatures(RefundProcedureSignatures { swap_id, .. }) => *swap_id,
            Msg::Abort(Abort { swap_id, .. }) => *swap_id,
            Msg::CoreArbitratingSetup(CoreArbitratingSetup { swap_id, .. }) => *swap_id,
            Msg::BuyProcedureSignature(BuyProcedureSignature { swap_id, .. }) => *swap_id,
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

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
pub enum Reveal {
    #[display("alice(..)")]
    AliceParameters(RevealAliceParameters),
    #[display("bob(..)")]
    BobParameters(RevealBobParameters),
    #[display("proof(..)")]
    Proof(RevealProof), // FIXME should be Msg::RevealProof(..)
}

#[derive(Clone, Debug, Display, From, StrictDecode, StrictEncode)]
#[display("{swap_id}, ..")]
pub struct TakeCommit {
    pub commit: Commit,
    pub public_offer: PublicOffer, // TODO: replace by public offer id
    pub swap_id: SwapId,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
pub enum Commit {
    #[display("alice(..)")]
    AliceParameters(CommitAliceParameters),
    #[display("bob(..)")]
    BobParameters(CommitBobParameters),
}
