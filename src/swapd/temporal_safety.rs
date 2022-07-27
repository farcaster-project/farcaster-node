use crate::Error;
use farcaster_core::blockchain::Blockchain;
use strict_encoding::{StrictDecode, StrictEncode};

pub type BlockHeight = u32;

#[derive(Debug, Clone, StrictEncode, StrictDecode)]
pub struct TemporalSafety {
    pub cancel_timelock: BlockHeight,
    pub punish_timelock: BlockHeight,
    pub race_thr: BlockHeight,
    pub btc_finality_thr: BlockHeight,
    pub xmr_finality_thr: BlockHeight,
    pub sweep_monero_thr: BlockHeight,
}

impl TemporalSafety {
    /// check if temporal params are in correct order
    pub fn valid_params(&self) -> Result<(), Error> {
        let btc_finality = self.btc_finality_thr;
        // let xmr_finality = self.xmr_finality_thr;
        let cancel = self.cancel_timelock;
        let punish = self.punish_timelock;
        let race = self.race_thr;
        if btc_finality < cancel
            && cancel < punish
            && btc_finality < race
            && punish > race
            && cancel > race
        // && btc_finality < xmr_finality
        {
            Ok(())
        } else {
            Err(Error::Farcaster(s!(
                "unsafe and invalid temporal parameters, timelocks, race and tx finality params"
            )))
        }
    }
    /// returns whether tx is final given the finality threshold set for the chain
    pub fn final_tx(&self, confs: u32, blockchain: Blockchain) -> bool {
        let finality_thr = match blockchain {
            Blockchain::Bitcoin => self.btc_finality_thr,
            Blockchain::Monero => self.xmr_finality_thr,
        };
        confs >= finality_thr
    }
    /// lock must be final, cancel cannot be raced, add + 1 to offset initial lock confirmation
    pub fn stop_funding_before_cancel(&self, lock_confirmations: u32) -> bool {
        self.final_tx(lock_confirmations, Blockchain::Bitcoin)
            && lock_confirmations > (self.cancel_timelock - self.race_thr + 1)
    }
    /// lock must be final, valid after lock_minedblock + cancel_timelock
    pub fn valid_cancel(&self, lock_confirmations: u32) -> bool {
        self.final_tx(lock_confirmations, Blockchain::Bitcoin)
            && lock_confirmations >= self.cancel_timelock
    }
    /// lock must be final, but buy shall not be raced with cancel
    pub fn safe_buy(&self, lock_confirmations: u32) -> bool {
        self.final_tx(lock_confirmations, Blockchain::Bitcoin)
            && lock_confirmations <= (self.cancel_timelock - self.race_thr)
    }
    /// cancel must be final, but refund shall not be raced with punish
    pub fn safe_refund(&self, cancel_confirmations: u32) -> bool {
        self.final_tx(cancel_confirmations, Blockchain::Bitcoin)
            && cancel_confirmations <= (self.punish_timelock - self.race_thr)
    }
    pub fn valid_punish(&self, cancel_confirmations: u32) -> bool {
        self.final_tx(cancel_confirmations, Blockchain::Bitcoin)
            && cancel_confirmations >= self.punish_timelock
    }
}
