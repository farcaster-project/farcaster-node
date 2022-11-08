use farcaster_core::{blockchain::Blockchain, transaction::TxLabel};
use strict_encoding::{NetworkDecode, NetworkEncode};

use crate::bus::{Outcome, Progress, StateTransition};

use super::{
    swap_state::{AliceState, BobState},
    syncer_client::SyncerState,
    temporal_safety::TemporalSafety,
    State,
};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub enum StateReport {
    StartA,
    CommitA,
    RevealA,
    RefundSigA {
        arb_block_height: u64,
        acc_block_height: u64,
        arb_locked: bool,
        acc_locked: bool,
        buy_published: bool,
        cancel_seen: bool,
        refund_seen: bool,
        overfunded: bool,
        arb_lock_confirmations: Option<u32>,
        acc_lock_confirmations: Option<u32>,
        blocks_until_cancel_possible: Option<i64>,
        cancel_confirmations: Option<u32>,
        blocks_until_punish_possible: Option<i64>,
        blocks_until_safe_buy: Option<u32>,
    },
    FinishA(Outcome),
    StartB,
    CommitB,
    RevealB,
    CorearbB {
        arb_block_height: u64,
        acc_block_height: u64,
        arb_locked: bool,
        acc_locked: bool,
        buy_published: bool,
        refund_seen: bool,
        arb_lock_confirmations: Option<u32>,
        acc_lock_confirmations: Option<u32>,
        blocks_until_cancel_possible: Option<i64>,
        cancel_confirmations: Option<u32>,
        blocks_until_refund: Option<i64>,
        blocks_until_punish_possible: Option<i64>,
    },
    BuySigB {
        arb_block_height: u64,
        acc_block_height: u64,
        buy_tx_seen: bool,
        arb_lock_confirmations: Option<u32>,
        acc_lock_confirmations: Option<u32>,
        blocks_until_cancel_possible: Option<i64>,
        cancel_confirmations: Option<u32>,
        blocks_until_refund: Option<i64>,
        blocks_until_punish_possible: Option<i64>,
        blocks_until_safe_monero_buy_sweep: Option<u32>,
    },
    FinishB(Outcome),
}

impl StateReport {
    pub fn new(
        state: &State,
        temp_safety: &TemporalSafety,
        syncer_state: &SyncerState,
    ) -> StateReport {
        match state {
            State::Alice(AliceState::StartA { .. }) => StateReport::StartA,
            State::Alice(AliceState::CommitA { .. }) => StateReport::CommitA,
            State::Alice(AliceState::RevealA { .. }) => StateReport::RevealA,
            State::Alice(AliceState::RefundSigA {
                btc_locked,
                xmr_locked,
                buy_published,
                cancel_seen,
                refund_seen,
                overfunded,
                ..
            }) => StateReport::RefundSigA {
                arb_block_height: syncer_state.bitcoin_height,
                acc_block_height: syncer_state.monero_height,
                arb_locked: *btc_locked,
                acc_locked: *xmr_locked,
                buy_published: *buy_published,
                cancel_seen: *cancel_seen,
                refund_seen: *refund_seen,
                overfunded: *overfunded,
                arb_lock_confirmations: syncer_state.get_confs(TxLabel::Lock),
                acc_lock_confirmations: syncer_state.get_confs(TxLabel::AccLock),
                blocks_until_cancel_possible: syncer_state
                    .get_confs(TxLabel::Lock)
                    .map(|confs| temp_safety.blocks_until_cancel(confs)),
                cancel_confirmations: syncer_state.get_confs(TxLabel::Cancel),
                blocks_until_punish_possible: syncer_state
                    .get_confs(TxLabel::Cancel)
                    .map(|confs| temp_safety.blocks_until_punish_after_cancel(confs)),
                blocks_until_safe_buy: syncer_state
                    .get_confs(TxLabel::Lock)
                    .map(|c| temp_safety.btc_finality_thr.checked_sub(c).unwrap_or(0)),
            },
            State::Alice(AliceState::FinishA(outcome)) => StateReport::FinishA(outcome.clone()),
            State::Bob(BobState::StartB { .. }) => StateReport::StartB,
            State::Bob(BobState::CommitB { .. }) => StateReport::CommitB,
            State::Bob(BobState::RevealB { .. }) => StateReport::RevealB,
            State::Bob(BobState::CorearbB { buy_tx_seen, .. }) => StateReport::CorearbB {
                buy_published: *buy_tx_seen,
                arb_block_height: syncer_state.bitcoin_height,
                acc_block_height: syncer_state.monero_height,
                arb_locked: temp_safety.final_tx(
                    syncer_state.get_confs(TxLabel::Lock).unwrap_or(0),
                    Blockchain::Bitcoin,
                ),
                acc_locked: temp_safety.final_tx(
                    syncer_state.get_confs(TxLabel::AccLock).unwrap_or(0),
                    Blockchain::Bitcoin,
                ),
                refund_seen: syncer_state.get_confs(TxLabel::Refund).is_some(),
                arb_lock_confirmations: syncer_state.get_confs(TxLabel::Lock),
                acc_lock_confirmations: syncer_state.get_confs(TxLabel::AccLock),
                blocks_until_cancel_possible: syncer_state
                    .get_confs(TxLabel::Lock)
                    .map(|confs| temp_safety.blocks_until_cancel(confs)),
                cancel_confirmations: syncer_state.get_confs(TxLabel::Cancel),
                blocks_until_refund: None,
                blocks_until_punish_possible: syncer_state
                    .get_confs(TxLabel::Cancel)
                    .map(|confs| temp_safety.blocks_until_punish_after_cancel(confs)),
            },
            State::Bob(BobState::BuySigB { buy_tx_seen, .. }) => StateReport::BuySigB {
                buy_tx_seen: *buy_tx_seen,
                arb_block_height: syncer_state.bitcoin_height,
                acc_block_height: syncer_state.monero_height,
                arb_lock_confirmations: syncer_state.get_confs(TxLabel::Lock),
                acc_lock_confirmations: syncer_state.get_confs(TxLabel::AccLock),
                blocks_until_cancel_possible: syncer_state
                    .get_confs(TxLabel::Lock)
                    .map(|confs| temp_safety.blocks_until_cancel(confs)),
                cancel_confirmations: syncer_state.get_confs(TxLabel::Cancel),
                blocks_until_refund: None,
                blocks_until_punish_possible: syncer_state
                    .get_confs(TxLabel::Cancel)
                    .map(|confs| temp_safety.blocks_until_punish_after_cancel(confs)),
                blocks_until_safe_monero_buy_sweep: syncer_state
                    .get_confs(TxLabel::AccLock)
                    .map(|c| temp_safety.sweep_monero_thr.checked_sub(c).unwrap_or(0)),
            },
            State::Bob(BobState::FinishB(outcome)) => StateReport::FinishB(outcome.clone()),
        }
    }

    pub fn generate_progress_update_or_transition(
        &self,
        new_state_report: &StateReport,
    ) -> Progress {
        if std::mem::discriminant(self) == std::mem::discriminant(new_state_report) {
            Progress::StateUpdate(new_state_report.clone())
        } else {
            Progress::StateTransition(StateTransition {
                old_state: self.clone(),
                new_state: new_state_report.clone(),
            })
        }
    }
}
