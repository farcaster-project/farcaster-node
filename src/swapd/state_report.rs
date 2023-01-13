// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use farcaster_core::{blockchain::Blockchain, transaction::TxLabel};
use strict_encoding::{NetworkDecode, NetworkEncode};

use crate::bus::{Progress, StateTransition};
use crate::swapd::temporal_safety::SWEEP_MONERO_THRESHOLD;

use super::{syncer_client::SyncerState, temporal_safety::TemporalSafety};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Display, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub struct StateReport {
    pub state: String,
    pub arb_block_height: u64,
    pub acc_block_height: u64,
    pub arb_locked: bool,
    pub acc_locked: bool,
    pub canceled: bool,
    pub buy_seen: bool,
    pub refund_seen: bool,
    pub overfunded: bool,
    pub arb_lock_confirmations: Option<u32>,
    pub acc_lock_confirmations: Option<u32>,
    pub cancel_confirmations: Option<u32>,
    pub refund_confirmations: Option<u32>,
    pub punish_confirmations: Option<u32>,
    pub buy_confirmations: Option<u32>,
    pub blocks_until_cancel_possible: Option<i64>,
    pub blocks_until_punish_possible: Option<i64>,
    pub blocks_until_safe_buy: Option<u32>,
    pub blocks_until_safe_monero_buy_sweep: Option<u32>,
}

impl StateReport {
    pub fn new(
        state: String,
        temp_safety: &TemporalSafety,
        syncer_state: &SyncerState,
    ) -> StateReport {
        StateReport {
            state,
            arb_block_height: syncer_state.bitcoin_height,
            acc_block_height: syncer_state.monero_height,
            arb_locked: temp_safety.final_tx(
                syncer_state.get_confs(TxLabel::Lock).unwrap_or(0),
                Blockchain::Bitcoin,
            ),
            acc_locked: temp_safety.final_tx(
                syncer_state.get_confs(TxLabel::AccLock).unwrap_or(0),
                Blockchain::Monero,
            ),
            canceled: temp_safety.final_tx(
                syncer_state.get_confs(TxLabel::Cancel).unwrap_or(0),
                Blockchain::Bitcoin,
            ),
            buy_seen: syncer_state.get_confs(TxLabel::Buy).is_some(),
            refund_seen: syncer_state.get_confs(TxLabel::Refund).is_some(),
            overfunded: false, // FIXME
            arb_lock_confirmations: syncer_state.get_confs(TxLabel::Lock),
            acc_lock_confirmations: syncer_state.get_confs(TxLabel::AccLock),
            cancel_confirmations: syncer_state.get_confs(TxLabel::Cancel),
            refund_confirmations: syncer_state.get_confs(TxLabel::Refund),
            punish_confirmations: syncer_state.get_confs(TxLabel::Punish),
            buy_confirmations: syncer_state.get_confs(TxLabel::Buy),
            blocks_until_cancel_possible: syncer_state
                .get_confs(TxLabel::Lock)
                .map(|confs| temp_safety.blocks_until_cancel(confs)),
            blocks_until_safe_buy: syncer_state
                .get_confs(TxLabel::Lock)
                .map(|c| temp_safety.arb_finality.saturating_sub(c)),
            blocks_until_punish_possible: syncer_state
                .get_confs(TxLabel::Cancel)
                .map(|confs| temp_safety.blocks_until_punish_after_cancel(confs)),
            blocks_until_safe_monero_buy_sweep: syncer_state
                .get_confs(TxLabel::AccLock)
                .map(|c| SWEEP_MONERO_THRESHOLD.saturating_sub(c)),
        }
    }

    pub fn generate_progress_update_or_transition(
        &self,
        new_state_report: &StateReport,
    ) -> Progress {
        if self.state == new_state_report.state {
            Progress::StateUpdate(new_state_report.clone())
        } else {
            Progress::StateTransition(StateTransition {
                old_state: self.clone(),
                new_state: new_state_report.clone(),
            })
        }
    }
}
