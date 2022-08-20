use farcaster_core::{
    negotiation::PublicOffer,
    role::{SwapRole, TradeRole},
    transaction::TxLabel,
};
use strict_encoding::{StrictDecode, StrictEncode};

use crate::{
    event::Event,
    rpc::{
        request::{Commit, InitSwap, Msg, Outcome, Params, Reveal, Tx},
        Request,
    },
    ServiceId,
};

use super::{runtime::PendingRequests, syncer_client::SyncerState};

#[derive(Display, Debug, Clone, StrictEncode, StrictDecode)]
pub enum AliceState {
    // #[display("Start: {0:#?} {1:#?}")]
    #[display("Start")]
    StartA { local_trade_role: TradeRole }, // local
    // #[display("Commit: {0}")]
    #[display("Commit")]
    CommitA {
        local_trade_role: TradeRole,
        local_params: Params,
        remote_params: Option<Params>,
        local_commit: Commit,
        remote_commit: Option<Commit>,
    }, // local, local, local, remote
    // #[display("Reveal: {0:#?}")]
    #[display("Reveal")]
    RevealA {
        local_params: Params,
        remote_commit: Commit,
        remote_params: Option<Params>,
        last_checkpoint_type: Option<SwapCheckpointType>,
    }, // local, remote, remote
    #[display("RefundSigs(xmr_locked({xmr_locked}), buy_pub({buy_published}), cancel_seen({cancel_seen}), refund_seen({refund_seen}), overfunded({overfunded}))")]
    RefundSigA {
        last_checkpoint_type: SwapCheckpointType,
        btc_locked: bool,
        xmr_locked: bool,
        buy_published: bool,
        cancel_seen: bool,
        refund_seen: bool,
        remote_params: Params,
        /* #[display("local_view_share({0})")] */
        local_params: Params,
        required_funding_amount: Option<u64>, // TODO: Should be monero::Amount
        overfunded: bool,
    },
    #[display("Finish({0})")]
    FinishA(Outcome),
}

#[derive(Display, Debug, Clone, StrictEncode, StrictDecode)]
pub enum BobState {
    // #[display("Start {0:#?} {1:#?}")]
    #[display("Start")]
    StartB { local_trade_role: TradeRole }, // local
    // #[display("Commit {0} {1}")]
    #[display("Commit")]
    CommitB {
        local_trade_role: TradeRole,
        local_params: Params,
        remote_params: Option<Params>,
        local_commit: Commit,
        remote_commit: Option<Commit>,
        b_address: bitcoin::Address,
        required_funding_amount: Option<bitcoin::Amount>,
    },
    // #[display("Reveal: {0:#?}")]
    #[display("Reveal")]
    RevealB {
        last_checkpoint_type: Option<SwapCheckpointType>,
        local_params: Params,
        remote_commit: Commit,
        b_address: bitcoin::Address,
        local_trade_role: TradeRole,
        remote_params: Option<Params>,
        required_funding_amount: Option<bitcoin::Amount>,
    }, // local, remote, local, ..missing, remote
    // #[display("CoreArb: {0:#?}")]
    #[display("CoreArb")]
    CorearbB {
        last_checkpoint_type: SwapCheckpointType,
        received_refund_procedure_signatures: bool,
        cancel_seen: bool,
        remote_params: Params,
        local_params: Params,
        b_address: bitcoin::Address,
    }, // lock (not signed), cancel_seen, remote
    #[display("BuySig")]
    BuySigB {
        buy_tx_seen: bool,
        last_checkpoint_type: SwapCheckpointType,
    },
    #[display("Finish({0})")]
    FinishB(Outcome),
}

#[derive(Display, Debug, Clone, StrictEncode, StrictDecode)]
#[display(inner)]
pub enum State {
    #[display("AliceState({0})")]
    Alice(AliceState),
    #[display("BobState({0})")]
    Bob(BobState),
}

#[derive(Display, Debug, Clone, StrictEncode, StrictDecode)]
#[display(inner)]
pub enum SwapCheckpointType {
    #[display("CheckpointBobPreLock")]
    CheckpointBobPreLock,
    #[display("CheckpointBobPreBuy")]
    CheckpointBobPreBuy,
    #[display("CheckpointAlicePreLock")]
    CheckpointAlicePreLock,
    #[display("CheckpointAlicePreBuy")]
    CheckpointAlicePreBuy,
}


// Awaiting enum variants encode the state abstractly by enforcing a set of
// predicates to validade the swap actually is in a given state, and thus is
// able to process the received event. It can be used as a state-machine to
// evolve through the protocol. Awaiting enum should eventually hold data
// extracted from the full state. The full state comprises all the input
// parameters to the contructor fn with. Each variant has an associated event
// processing fn. When calling the associated event processing fn, Awaiting then
// can deliver the required data to the processing fn. Currently the full
// runtime is available to the associated processing fns and thus this is not
// needed, although many unwraps and unneeded checks are still in there.
/// Awaiting enum variants encode the state abstractly by enforcing a set of
/// predicates to validade the swap actually is in a given state
pub enum Awaiting {
    // Msg
    TakerP2pMakerCommit,
    MakerTakerP2pRevealProof,
    BobP2pRevealAlice,
    MakerTakerP2pReveal,
    AliceP2pCoreArb,
    BobP2pRefundSigs,
    AliceP2pBuySig,
    // Ctl
    TakerCtlTakeSwap,
    MakerCtlRevealProof,
    MakerTakerCtlFundingUpdated,
    MakerCtlMakeSwap,
    BobCtlCoreArb,
    AliceCtlRefundSigs,
    BobCtlBuySig,
    BobCtlTxLock,
    AliceBobCtlTx,
    AliceBobCtlSweepMonero,
    AliceBobCtlTerminate,
    AliceBobCtlSweepBitcoin,
    AliceBobCtlRpcAbortSimple,
    BobCtlRpcAbortBob,
    AliceBobCtlRpcAbortBlocked,
    AliceBobRpcGetInfo,
    AliceBobCtlPeerReconnected,
    AliceBobCtlCheckpoint,

    // else
    Unknown,
}

impl Awaiting {
    pub fn with(
        state: &State,
        event: &Event<Request>,
        syncer_state: Option<&SyncerState>,
        pending_requests: Option<&PendingRequests>,
    ) -> Self {
        // Msg bus
        if state.p_t_p2p_maker_commit(&event) {
            Awaiting::TakerP2pMakerCommit
        } else if state.p_mt_p2p_reveal_proof(&event) {
            Awaiting::MakerTakerP2pRevealProof
        } else if syncer_state.is_some() && state.p_b_reveal_alice(&event, syncer_state.unwrap()) {
            Awaiting::BobP2pRevealAlice
        } else if syncer_state.is_some() && state.p_mt_p2p_reveal(event, syncer_state.unwrap()) {
            Awaiting::MakerTakerP2pReveal
        } else if state.p_a_p2p_core_arb(event) {
            Awaiting::AliceP2pCoreArb
        } else if state.p_b_p2p_refund_sigs(event) {
            Awaiting::BobP2pRefundSigs
        } else if state.p_a_p2p_buy_sig(event) {
            Awaiting::AliceP2pBuySig
        }
        // Ctl bus
        else if state.p_t_ctl_take_swap(event) {
            Awaiting::TakerCtlTakeSwap
        } else if state.p_tm_ctl_reveal_proof(event) {
            Awaiting::MakerCtlRevealProof
        } else if state.p_m_ctl_make_swap(event) {
            Awaiting::MakerCtlMakeSwap
        } else if pending_requests.is_some()
            && state.p_mt_ctl_funding_updated(event, pending_requests.unwrap())
        {
            Awaiting::MakerTakerCtlFundingUpdated
        } else if state.p_b_ctl_core_arb(event) {
            Awaiting::BobCtlCoreArb
        } else if state.p_a_ctl_refund_sigs(event) {
            Awaiting::AliceCtlRefundSigs
        } else if syncer_state.is_some() && state.p_b_ctl_buy_sig(event, syncer_state.unwrap()) {
            Awaiting::BobCtlBuySig
        } else if state.p_b_ctl_tx_lock(event) {
            Awaiting::BobCtlTxLock
        } else if state.p_ab_ctl_tx(event) {
            Awaiting::AliceBobCtlTx
        } else if state.p_ab_ctl_sweep_monero(event) {
            Awaiting::AliceBobCtlSweepMonero
        } else if state.p_ab_ctl_sweep_bitcoin(event) {
            Awaiting::AliceBobCtlSweepBitcoin
        } else if state.p_ab_ctl_terminate(event) {
            Awaiting::AliceBobCtlTerminate
        } else if state.p_ab_ctl_peer_reconnected(event) {
            Awaiting::AliceBobCtlPeerReconnected
        } else if state.p_ab_ctl_checkpoint(event) {
            Awaiting::AliceBobCtlCheckpoint
        }
        // Ctl + Rpc buses
        else if state.p_ab_ctl_rpc_abort_simple(event) {
            Awaiting::AliceBobCtlRpcAbortSimple
        } else if state.p_b_ctl_rpc_abort_bob(event) {
            Awaiting::BobCtlRpcAbortBob
        } else if state.p_ab_ctl_rpc_abort_blocked(event) {
            Awaiting::AliceBobCtlRpcAbortBlocked
        }
        // Rpc bus
        else if state.p_ab_rpc_get_info(event) {
            Awaiting::AliceBobRpcGetInfo
        }
        // Unknown
        else {
            error!("Could not match this event and state to any known condition");
            Awaiting::Unknown
        }
    }
}

// The state impl is not public and may contain code not used yet, we can relax the linter and
// allow dead code.
#[allow(dead_code)]
impl State {
    // p stands for predicate, TODO add more predicates such as source, dest,
    // syncer_state, txs. FIXME for events that require data, e.g., b_address,
    // extract data. Probably segregate the predicate from the event.message.
    // Needed for the automata that runs not in response to events, but because
    // it already has all the needed preconditions and can perform actions
    // independently
    pub fn p_t_p2p_maker_commit(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::Protocol(Msg::MakerCommit(_)))
            && self.commit()
            && self.trade_role() == Some(TradeRole::Taker)
            && self.remote_commit().is_none()
    }
    pub fn p_mt_p2p_reveal_proof(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::Protocol(Msg::Reveal(Reveal::Proof(_))))
            && !self.p_tm_ctl_reveal_proof(ev)
    }
    pub fn p_b_reveal_alice(&self, ev: &Event<Request>, syncer_state: &SyncerState) -> bool {
        matches!(
            ev.message,
            Request::Protocol(Msg::Reveal(Reveal::AliceParameters(..)))
        ) && self.swap_role() == SwapRole::Bob
            && (self.b_address().is_none() || syncer_state.btc_fee_estimate_sat_per_kvb.is_none())
    }
    pub fn p_mt_p2p_reveal(&self, ev: &Event<Request>, syncer_state: &SyncerState) -> bool {
        matches!(ev.message, Request::Protocol(Msg::Reveal(_)))
            && !matches!(ev.message, Request::Protocol(Msg::Reveal(Reveal::Proof(_)))) // FIXME: remove when Reveal Proof split
            && self.remote_commit().is_some()
            && (self.commit() || self.reveal())
            && {
                match (
                    self.swap_role(),
                    self.b_address().is_some(),
                    syncer_state.btc_fee_estimate_sat_per_kvb.is_some(),
                ) {
                    (SwapRole::Bob, true, true) => true,
                    (SwapRole::Bob, ..) => false,
                    (SwapRole::Alice, ..) => true,
                }
            }
    }
    pub fn p_a_p2p_core_arb(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::Protocol(Msg::CoreArbitratingSetup(_)))
            && self.swap_role() == SwapRole::Alice
            && self.reveal()
    }
    pub fn p_b_p2p_refund_sigs(&self, ev: &Event<Request>) -> bool {
        matches!(
            ev.message,
            Request::Protocol(Msg::RefundProcedureSignatures(_))
        ) && self.b_core_arb()
    }

    pub fn p_a_p2p_buy_sig(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::Protocol(Msg::BuyProcedureSignature(_)))
            && self.a_refundsig()
            // && self.a_sup_checkpoint_pre_buy() // FIXME turn on, off for not changing behavior on refactor
            && !self.a_overfunded()
    }

    // Ctl
    pub fn p_t_ctl_take_swap(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::TakeSwap(_))
    }
    pub fn p_tm_ctl_reveal_proof(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::Protocol(Msg::Reveal(Reveal::Proof(_))))
            && matches!(ev.source, ServiceId::Wallet)
            && matches!(ev.service, ServiceId::Swap(_))
            && self.commit()
            && self.remote_commit().is_some()
    }
    pub fn p_m_ctl_make_swap(&self, ev: &Event<Request>) -> bool {
        matches!(
            ev.message,
            Request::MakeSwap(InitSwap {
                remote_commit: Some(_),
                ..
            })
        ) && self.start()
    }
    pub fn p_mt_ctl_funding_updated(
        &self,
        ev: &Event<Request>,
        pending_requests: &PendingRequests, // FIXME pending_request should be PendingEvents
    ) -> bool {
        matches!(ev.message, Request::FundingUpdated)
            && ev.source == ServiceId::Wallet
            && ((self.trade_role() == Some(TradeRole::Taker) && self.reveal())
                || (self.trade_role() == Some(TradeRole::Maker) && self.commit()))
            && pending_requests.contains_key(&ev.source)
            && pending_requests
                .get(&ev.source)
                .map(|reqs| reqs.len() == 2)
                .unwrap()
    }

    pub fn p_b_ctl_core_arb(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::Protocol(Msg::CoreArbitratingSetup(_)))
            && self.reveal()
            && self.swap_role() == SwapRole::Bob
            && self.remote_params().is_some()
            && self.local_params().is_some()
    }

    pub fn p_a_ctl_refund_sigs(&self, ev: &Event<Request>) -> bool {
        matches!(
            ev.message,
            Request::Protocol(Msg::RefundProcedureSignatures(_))
        ) && self.reveal()
            && self.swap_role() == SwapRole::Alice
            && self.remote_params().is_some()
            && self.local_params().is_some()
    }

    pub fn p_b_ctl_buy_sig(&self, ev: &Event<Request>, syncer_state: &SyncerState) -> bool {
        matches!(ev.message, Request::Protocol(Msg::BuyProcedureSignature(_)))
            && self.b_core_arb()
            && !syncer_state.tasks.txids.contains_key(&TxLabel::Buy)
    }

    pub fn p_b_ctl_tx_lock(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::Tx(Tx::Lock(_)))
            && self.b_core_arb()
            && self.local_params().is_some()
            && self.remote_params().is_some()
    }
    pub fn p_ab_ctl_tx(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::Tx(_)) && !self.p_b_ctl_tx_lock(ev)
    }

    pub fn p_ab_ctl_sweep_monero(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::SweepMoneroAddress(_)) && ev.source == ServiceId::Wallet
    }

    pub fn p_ab_ctl_terminate(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::Terminate) && ev.source == ServiceId::Farcasterd
    }

    pub fn p_ab_ctl_sweep_bitcoin(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::SweepBitcoinAddress(_))
    }
    pub fn p_ab_ctl_rpc_abort_simple(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::AbortSwap)
            && (self.b_start()
                || self.a_start()
                || self.a_commit()
                || self.a_reveal()
                || (self.a_refundsig() && !self.a_btc_locked()))
    }
    pub fn p_b_ctl_rpc_abort_bob(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::AbortSwap)
            && (self.b_commit()
                || self.b_reveal()
                || (self.b_core_arb() && !self.b_received_refund_procedure_signatures()))
    }
    pub fn p_ab_ctl_rpc_abort_blocked(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::AbortSwap)
            && !self.p_ab_ctl_rpc_abort_simple(ev)
            && !self.p_b_ctl_rpc_abort_bob(ev)
    }
    pub fn p_ab_rpc_get_info(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::GetInfo)
    }
    pub fn p_ab_ctl_peer_reconnected(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::PeerdReconnected)
    }
    pub fn p_ab_ctl_checkpoint(&self, ev: &Event<Request>) -> bool {
        matches!(ev.message, Request::Checkpoint(_))
    }
    pub fn swap_role(&self) -> SwapRole {
        match self {
            State::Alice(_) => SwapRole::Alice,
            State::Bob(_) => SwapRole::Bob,
        }
    }
    pub fn remote_params(&self) -> Option<Params> {
        match self {
            State::Alice(AliceState::CommitA { remote_params, .. })
            | State::Alice(AliceState::RevealA { remote_params, .. })
            | State::Bob(BobState::CommitB { remote_params, .. })
            | State::Bob(BobState::RevealB { remote_params, .. }) => remote_params.clone(),

            State::Alice(AliceState::RefundSigA { remote_params, .. })
            | State::Bob(BobState::CorearbB { remote_params, .. }) => Some(remote_params.clone()),

            _ => None,
        }
    }
    pub fn sup_remote_params(&mut self, params: Params) -> bool {
        match self {
            State::Alice(AliceState::CommitA { remote_params, .. })
            | State::Alice(AliceState::RevealA { remote_params, .. })
            | State::Bob(BobState::CommitB { remote_params, .. })
            | State::Bob(BobState::RevealB { remote_params, .. })
                if remote_params.is_none() =>
            {
                *remote_params = Some(params);
                true
            }
            _ => false,
        }
    }
    pub fn a_btc_locked(&self) -> bool {
        if let State::Alice(AliceState::RefundSigA { btc_locked, .. }) = self {
            *btc_locked
        } else {
            false
        }
    }
    pub fn a_xmr_locked(&self) -> bool {
        if let State::Alice(AliceState::RefundSigA { xmr_locked, .. }) = self {
            *xmr_locked
        } else {
            false
        }
    }
    pub fn a_buy_published(&self) -> bool {
        if let State::Alice(AliceState::RefundSigA { buy_published, .. }) = self {
            *buy_published
        } else {
            false
        }
    }
    pub fn a_refund_seen(&self) -> bool {
        if let State::Alice(AliceState::RefundSigA { refund_seen, .. }) = self {
            *refund_seen
        } else {
            false
        }
    }
    pub fn cancel_seen(&self) -> bool {
        if let State::Bob(BobState::CorearbB { cancel_seen, .. })
        | State::Alice(AliceState::RefundSigA { cancel_seen, .. }) = self
        {
            *cancel_seen
        } else {
            false
        }
    }
    pub fn sup_cancel_seen(&mut self) -> bool {
        match self {
            State::Alice(AliceState::RefundSigA { cancel_seen, .. })
            | State::Bob(BobState::CorearbB { cancel_seen, .. }) => {
                *cancel_seen = true;
                true
            }
            _ => false,
        }
    }
    pub fn sup_received_refund_procedure_signatures(&mut self) -> bool {
        match self {
            State::Bob(BobState::CorearbB {
                received_refund_procedure_signatures,
                ..
            }) => {
                *received_refund_procedure_signatures = true;
                true
            }
            _ => false,
        }
    }
    pub fn b_received_refund_procedure_signatures(&self) -> bool {
        if let State::Bob(BobState::CorearbB {
            received_refund_procedure_signatures,
            ..
        }) = self
        {
            *received_refund_procedure_signatures
        } else {
            false
        }
    }
    pub fn b_required_funding_amount(&self) -> Option<bitcoin::Amount> {
        match self {
            State::Bob(BobState::CommitB {
                required_funding_amount,
                ..
            })
            | State::Bob(BobState::RevealB {
                required_funding_amount,
                ..
            }) => required_funding_amount.clone(),
            _ => None,
        }
    }
    pub fn a_start(&self) -> bool {
        matches!(self, State::Alice(AliceState::StartA { .. }))
    }
    pub fn a_commit(&self) -> bool {
        matches!(self, State::Alice(AliceState::CommitA { .. }))
    }
    pub fn a_reveal(&self) -> bool {
        matches!(self, State::Alice(AliceState::RevealA { .. }))
    }
    pub fn b_start(&self) -> bool {
        matches!(self, State::Bob(BobState::StartB { .. }))
    }
    pub fn b_commit(&self) -> bool {
        matches!(self, State::Bob(BobState::CommitB { .. }))
    }
    pub fn b_reveal(&self) -> bool {
        matches!(self, State::Bob(BobState::RevealB { .. }))
    }
    pub fn b_core_arb(&self) -> bool {
        matches!(self, State::Bob(BobState::CorearbB { .. }))
    }
    pub fn b_buy_sig(&self) -> bool {
        matches!(self, State::Bob(BobState::BuySigB { .. }))
    }
    pub fn b_outcome_abort(&self) -> bool {
        matches!(self, State::Bob(BobState::FinishB(Outcome::Abort)))
    }
    pub fn remote_commit(&self) -> Option<&Commit> {
        match self {
            State::Alice(AliceState::CommitA { remote_commit, .. })
            | State::Bob(BobState::CommitB { remote_commit, .. }) => remote_commit.as_ref(),
            State::Alice(AliceState::RevealA { remote_commit, .. })
            | State::Bob(BobState::RevealB { remote_commit, .. }) => Some(remote_commit),
            _ => None,
        }
    }
    pub fn local_params(&self) -> Option<&Params> {
        match self {
            State::Alice(AliceState::CommitA { local_params, .. })
            | State::Bob(BobState::CommitB { local_params, .. })
            | State::Alice(AliceState::RevealA { local_params, .. })
            | State::Alice(AliceState::RefundSigA { local_params, .. })
            | State::Bob(BobState::RevealB { local_params, .. })
            | State::Bob(BobState::CorearbB { local_params, .. }) => Some(local_params),
            _ => None,
        }
    }
    pub fn b_address(&self) -> Option<&bitcoin::Address> {
        match self {
            State::Bob(BobState::CommitB { b_address, .. })
            | State::Bob(BobState::RevealB { b_address, .. })
            | State::Bob(BobState::CorearbB { b_address, .. }) => Some(b_address),
            _ => None,
        }
    }
    pub fn local_commit(&self) -> Option<&Commit> {
        match self {
            State::Bob(BobState::CommitB { local_commit, .. })
            | State::Alice(AliceState::CommitA { local_commit, .. }) => Some(local_commit),
            _ => None,
        }
    }
    pub fn commit(&self) -> bool {
        matches!(
            self,
            State::Alice(AliceState::CommitA { .. }) | State::Bob(BobState::CommitB { .. })
        )
    }
    pub fn reveal(&self) -> bool {
        matches!(
            self,
            State::Alice(AliceState::RevealA { .. }) | State::Bob(BobState::RevealB { .. })
        )
    }
    pub fn last_checkpoint_type(&self) -> Option<SwapCheckpointType> {
        match self {
            State::Alice(AliceState::RevealA {
                last_checkpoint_type,
                ..
            })
            | State::Bob(BobState::RevealB {
                last_checkpoint_type,
                ..
            }) => last_checkpoint_type.clone(),
            State::Alice(AliceState::RefundSigA {
                last_checkpoint_type,
                ..
            })
            | State::Bob(BobState::CorearbB {
                last_checkpoint_type,
                ..
            })
            | State::Bob(BobState::BuySigB {
                last_checkpoint_type,
                ..
            }) => Some(last_checkpoint_type.clone()),
            _ => None,
        }
    }
    pub fn a_refundsig(&self) -> bool {
        matches!(self, State::Alice(AliceState::RefundSigA { .. }))
    }
    pub fn a_required_funding_amount(&self) -> Option<u64> {
        match self {
            State::Alice(AliceState::RefundSigA {
                required_funding_amount,
                ..
            }) => required_funding_amount.clone(),
            _ => None,
        }
    }
    pub fn a_overfunded(&self) -> bool {
        matches!(
            self,
            State::Alice(AliceState::RefundSigA {
                overfunded: true,
                ..
            })
        )
    }
    pub fn b_buy_tx_seen(&self) -> bool {
        if !self.b_buy_sig() {
            return false;
        }
        match self {
            State::Bob(BobState::BuySigB { buy_tx_seen, .. }) => *buy_tx_seen,
            _ => unreachable!("conditional early return"),
        }
    }
    /// returns whether safe to cancel given swap role & current stage of swap protocol
    pub fn safe_cancel(&self) -> bool {
        if self.finish() || self.cancel_seen() || self.a_refund_seen() {
            return false;
        }
        match self.swap_role() {
            SwapRole::Alice => self.a_refundsig() && !self.a_buy_published() && !self.cancel_seen(),
            SwapRole::Bob => {
                (self.b_core_arb() || self.b_buy_sig())
                    && !self.b_buy_tx_seen()
                    && !self.cancel_seen()
            }
        }
    }
    pub fn start(&self) -> bool {
        matches!(
            self,
            State::Alice(AliceState::StartA { .. }) | State::Bob(BobState::StartB { .. })
        )
    }
    pub fn finish(&self) -> bool {
        matches!(
            self,
            State::Alice(AliceState::FinishA(..)) | State::Bob(BobState::FinishB(..))
        )
    }
    pub fn trade_role(&self) -> Option<TradeRole> {
        match self {
            State::Alice(AliceState::StartA {
                local_trade_role, ..
            })
            | State::Bob(BobState::StartB {
                local_trade_role, ..
            })
            | State::Alice(AliceState::CommitA {
                local_trade_role, ..
            })
            | State::Bob(BobState::CommitB {
                local_trade_role, ..
            })
            | State::Bob(BobState::RevealB {
                local_trade_role, ..
            }) => Some(*local_trade_role),
            _ => None,
        }
    }
    pub fn sup_start_to_commit(
        self,
        local_commit: Commit,
        local_params: Params,
        funding_address: Option<bitcoin::Address>,
        remote_commit: Option<Commit>,
    ) -> Self {
        if !self.start() {
            error!("Not on Start state, not updating state");
            return self;
        }
        let remote_params = None;
        match (self, funding_address) {
            (State::Bob(BobState::StartB{local_trade_role, ..}), Some(b_address)) => {
                State::Bob(BobState::CommitB {
                        local_trade_role,
                        local_params,
                        local_commit,
                        remote_commit,
                        remote_params,
                        b_address,
                        required_funding_amount: None,
                    },
                )
            }
            (State::Alice(AliceState::StartA{local_trade_role, ..}), None) => {
                State::Alice(AliceState::CommitA{
                    local_trade_role,
                    local_params,
                    local_commit,
                    remote_commit,
                    remote_params,
                })

            }
            _ => unreachable!(
                "state conditional enforces state is Start: Expects Start state, and funding_address"
            ),
        }
    }
    pub fn sup_commit_to_reveal(self) -> Self {
        if !self.commit() {
            error!("Not on Commit state, not updating state");
            return self;
        }
        if self.remote_commit().is_none() {
            error!("remote commit should be set already");
            return self;
        }
        match self {
            State::Alice(AliceState::CommitA {
                local_params,
                remote_commit: Some(remote_commit),
                remote_params,
                ..
            }) => State::Alice(AliceState::RevealA {
                local_params,
                remote_commit,
                remote_params,
                last_checkpoint_type: None,
            }),

            State::Bob(BobState::CommitB {
                local_params,
                remote_commit: Some(remote_commit),
                local_trade_role,
                remote_params,
                b_address,
                required_funding_amount,
                ..
            }) => State::Bob(BobState::RevealB {
                local_params,
                remote_commit,
                b_address,
                local_trade_role,
                remote_params,
                last_checkpoint_type: None,
                required_funding_amount,
            }),

            _ => unreachable!("checked state on pattern to be Commit"),
        }
    }
    pub fn t_sup_remote_commit(&mut self, commit: Commit) -> bool {
        if !self.commit() {
            error!("Not on Commit state, not updating state");
            return false;
        }
        if self.remote_commit().is_some() {
            error!("remote commit already set");
            return false;
        }

        match self {
            State::Alice(AliceState::CommitA { remote_commit, .. })
            | State::Bob(BobState::CommitB { remote_commit, .. }) => {
                *remote_commit = Some(commit);
            }
            _ => unreachable!("checked state on pattern to be Commit"),
        }
        true
    }
    /// Update Bob BuySig state from XMR unlocked to locked state
    pub fn b_sup_buysig_buy_tx_seen(&mut self) -> bool {
        if !self.b_buy_sig() {
            error!(
                "Wrong state, not updating. Expected BuySig, found {}",
                &*self
            );
            return false;
        } else if self.b_buy_tx_seen() {
            error!("Buy tx was previously seen, not updating state");
            return false;
        }
        match self {
            State::Bob(BobState::BuySigB { buy_tx_seen, .. }) if !(*buy_tx_seen) => {
                *buy_tx_seen = true;
                true
            }
            _ => unreachable!("checked state"),
        }
    }
    /// Update Bob with the required Monero funding amount
    pub fn b_sup_required_funding_amount(&mut self, amount: bitcoin::Amount) -> bool {
        match self {
            State::Bob(BobState::CommitB {
                required_funding_amount,
                ..
            })
            | State::Bob(BobState::RevealB {
                required_funding_amount,
                ..
            }) => {
                if required_funding_amount.is_none() {
                    *required_funding_amount = Some(amount);
                    true
                } else {
                    trace!("required funding amount was already set");
                    false
                }
            }
            _ => {
                error!("Not on CommitB or RevealB state");
                false
            }
        }
    }
    /// Update Alice RefundSig state from BTC unlocked to locked state
    pub fn a_sup_refundsig_btclocked(&mut self) -> bool {
        if let State::Alice(AliceState::RefundSigA { btc_locked, .. }) = self {
            if !*btc_locked {
                trace!("setting btc_locked");
                *btc_locked = true;
                true
            } else {
                trace!("btc_locked was already set to true");
                false
            }
        } else {
            error!("Not on RefundSig state");
            false
        }
    }
    /// Update Alice RefundSig state from overfunded=false to overfunded=true
    pub fn a_sup_overfunded(&mut self) -> bool {
        if let State::Alice(AliceState::RefundSigA { overfunded, .. }) = self {
            if !*overfunded {
                trace!("setting overfunded");
                *overfunded = true;
                true
            } else {
                warn!("overfunded was already set to true");
                false
            }
        } else {
            error!("Not on RefundSig state");
            false
        }
    }
    /// Update Alice RefundSig state from XMR unlocked to locked state
    pub fn a_sup_refundsig_xmrlocked(&mut self) -> bool {
        if let State::Alice(AliceState::RefundSigA { xmr_locked, .. }) = self {
            if !*xmr_locked {
                trace!("setting xmr_locked");
                *xmr_locked = true;
                true
            } else {
                trace!("xmr_locked was already set to true");
                false
            }
        } else {
            error!("Not on RefundSig state");
            false
        }
    }
    /// Update Alice RefundSig state with the required Monero funding amount
    pub fn a_sup_required_funding_amount(&mut self, amount: monero::Amount) -> bool {
        if let State::Alice(AliceState::RefundSigA {
            required_funding_amount,
            ..
        }) = self
        {
            if required_funding_amount.is_none() {
                *required_funding_amount = Some(amount.as_pico());
                true
            } else {
                warn!("required funding amount was already set");
                false
            }
        } else {
            error!("Not on RefundSig state");
            false
        }
    }

    pub fn a_sup_refundsig_refund_seen(&mut self) -> bool {
        if let State::Alice(AliceState::RefundSigA { refund_seen, .. }) = self {
            if !*refund_seen {
                trace!("setting refund_seen");
                *refund_seen = true;
                true
            } else {
                error!("refund_seen was already set to true");
                false
            }
        } else {
            error!("Not on RefundSig state");
            false
        }
    }
    pub fn a_sup_checkpoint_pre_lock(&mut self) -> bool {
        if let State::Alice(AliceState::RevealA {
            last_checkpoint_type,
            ..
        }) = self
        {
            if last_checkpoint_type.is_none() {
                *last_checkpoint_type = Some(SwapCheckpointType::CheckpointAlicePreLock);
                true
            } else {
                debug!("checkpoint alice pre lock already set");
                false
            }
        } else {
            error!("Not on RevealA state");
            false
        }
    }
    pub fn a_sup_checkpoint_pre_buy(&mut self) -> bool {
        if let State::Alice(AliceState::RefundSigA {
            last_checkpoint_type,
            ..
        }) = self
        {
            if let SwapCheckpointType::CheckpointAlicePreLock = *last_checkpoint_type {
                *last_checkpoint_type = SwapCheckpointType::CheckpointAlicePreBuy;
                true
            } else {
                debug!("checkpoint alice pre buy already set");
                false
            }
        } else {
            error!("Not on RefundSigA state");
            false
        }
    }
    pub fn b_sup_checkpoint_pre_lock(&mut self) -> bool {
        if let State::Bob(BobState::RevealB {
            last_checkpoint_type,
            ..
        }) = self
        {
            if last_checkpoint_type.is_none() {
                *last_checkpoint_type = Some(SwapCheckpointType::CheckpointBobPreLock);
                true
            } else {
                debug!("checkpoint bob pre lock already set");
                false
            }
        } else {
            error!("Not on RevealB state");
            false
        }
    }
    pub fn b_sup_checkpoint_pre_buy(&mut self) -> bool {
        if let State::Bob(BobState::CorearbB {
            last_checkpoint_type,
            ..
        }) = self
        {
            if let SwapCheckpointType::CheckpointBobPreLock = *last_checkpoint_type {
                *last_checkpoint_type = SwapCheckpointType::CheckpointBobPreBuy;
                true
            } else {
                debug!("checkpoint bob pre buy already set");
                false
            }
        } else {
            error!("Not on CoreArbB state");
            false
        }
    }
}
