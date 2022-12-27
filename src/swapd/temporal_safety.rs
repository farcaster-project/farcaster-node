// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use crate::Error;
use farcaster_core::blockchain::Blockchain;
use strict_encoding::{StrictDecode, StrictEncode};

/// Represent a blockchain height
pub type BlockHeight = u32;

/// Represent a block length or a block number
pub type BlockSpan = u32;

/// The minimum number of block confirmations required before sweeping
pub const SWEEP_MONERO_THRESHOLD: u32 = 10;

/// List of parameters used to determined if a transaction should be considered final or not and if
/// it is safe to broadcast a transaction given the timelocks and confirmations of other
/// transactions.
#[derive(Debug, Clone, StrictEncode, StrictDecode)]
pub struct TemporalSafety {
    /// First timelock in the protocol, used to abort the swap instead of buying tx
    pub cancel_timelock: BlockHeight,
    /// Second timelock in the protocol, if this timelock is reached Bob miss-behaved and will lose
    /// money
    pub punish_timelock: BlockHeight,
    /// Avoid broadcasting a transaction if a race can happen with the next available execution
    /// fork in # blocks
    pub safety: BlockSpan,
    /// Number of confirmation required for the arbitrating blockchain to consider a tx final
    pub arb_finality: BlockSpan,
    /// Number of confirmation required for the accordant blockchain to consider a tx final
    pub acc_finality: BlockSpan,
}

impl TemporalSafety {
    /// Validate if temporal parameters are coherent
    pub fn valid_params(&self) -> Result<(), Error> {
        let finality = self.arb_finality;
        let cancel = self.cancel_timelock;
        let punish = self.punish_timelock;
        let race = self.safety;
        if finality < cancel && cancel < punish && finality < race && punish > race && cancel > race
        {
            Ok(())
        } else {
            Err(Error::Farcaster(s!(
                "Unsafe or invalid temporal parameters between: timelocks, finality, and safety"
            )))
        }
    }

    /// Returns whether tx is final given the finality threshold set for the chain
    pub fn final_tx(&self, confs: u32, blockchain: Blockchain) -> bool {
        let finality = match blockchain {
            Blockchain::Bitcoin => self.arb_finality,
            Blockchain::Monero => self.acc_finality,
        };
        confs >= finality
    }

    /// Lock must be final, cancel cannot be raced, add + 1 to offset initial lock confirmation
    pub fn stop_funding_before_cancel(&self, lock_confirmations: u32) -> bool {
        self.final_tx(lock_confirmations, Blockchain::Bitcoin)
            && lock_confirmations > (self.cancel_timelock - self.safety + 1)
    }

    // Blocks remaining until funding will be stopped for safety, because it is too close to
    // cancel. Adds the same +1 offset as in stop_funding_before_cancel
    pub fn blocks_until_stop_funding(&self, lock_confirmations: u32) -> i64 {
        self.cancel_timelock as i64 - (self.safety as i64 + 1 + lock_confirmations as i64)
    }

    /// Lock must be final, valid after lock_minedblock + cancel_timelock
    pub fn valid_cancel(&self, lock_confirmations: u32) -> bool {
        self.final_tx(lock_confirmations, Blockchain::Bitcoin)
            && lock_confirmations >= self.cancel_timelock
    }

    /// Blocks remaining until cancel, copies logic from valid_cancel
    pub fn blocks_until_cancel(&self, lock_confirmations: u32) -> i64 {
        self.cancel_timelock as i64 - lock_confirmations as i64
    }

    /// Lock must be final, but buy shall not be raced with cancel
    pub fn safe_buy(&self, lock_confirmations: u32) -> bool {
        self.final_tx(lock_confirmations, Blockchain::Bitcoin)
            && lock_confirmations <= (self.cancel_timelock - self.safety)
    }

    /// Cancel must be final, but refund shall not be raced with punish
    pub fn safe_refund(&self, cancel_confirmations: u32) -> bool {
        self.final_tx(cancel_confirmations, Blockchain::Bitcoin)
            && cancel_confirmations <= (self.punish_timelock - self.safety)
    }

    /// Cancel must be final, valid after cancel_confirmations > punish_timelock
    pub fn valid_punish(&self, cancel_confirmations: u32) -> bool {
        self.final_tx(cancel_confirmations, Blockchain::Bitcoin)
            && cancel_confirmations >= self.punish_timelock
    }

    /// Blocks remaning until punish, copies logic from valid_punish
    pub fn blocks_until_punish_after_cancel(&self, cancel_confirmations: u32) -> i64 {
        self.punish_timelock as i64 - cancel_confirmations as i64
    }

    pub fn block_height_reorg_lower_bound(
        &self,
        blockchain: Blockchain,
        current_height: u64,
    ) -> u64 {
        let finality_thr = match blockchain {
            Blockchain::Bitcoin => self.arb_finality,
            Blockchain::Monero => self.acc_finality,
        };
        current_height.saturating_sub(finality_thr as u64)
    }
}
