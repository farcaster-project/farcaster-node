// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use std::collections::HashSet;

use farcaster_core::{blockchain::Blockchain, swap::SwapId};
use strict_encoding::{NetworkDecode, NetworkEncode};

use crate::bus::Outcome;
use crate::LogStyle;

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Default, Clone, PartialEq, Eq, Debug, NetworkEncode, NetworkDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub struct Stats {
    pub success: u64,
    pub refund: u64,
    pub punish: u64,
    pub abort: u64,
    pub initialized: u64,
    pub awaiting_funding_btc: HashSet<SwapId>,
    pub awaiting_funding_xmr: HashSet<SwapId>,
    pub funded_xmr: u64,
    pub funded_btc: u64,
    pub funding_canceled_xmr: u64,
    pub funding_canceled_btc: u64,
}

impl Stats {
    pub fn incr_outcome(&mut self, outcome: &Outcome) {
        match outcome {
            Outcome::SuccessSwap => self.success += 1,
            Outcome::FailureRefund => self.refund += 1,
            Outcome::FailurePunish => self.punish += 1,
            Outcome::FailureAbort => self.abort += 1,
        };
    }

    pub fn incr_initiated(&mut self) {
        self.initialized += 1;
    }

    pub fn incr_awaiting_funding(&mut self, blockchain: &Blockchain, swapid: SwapId) {
        let newly_inserted = match blockchain {
            Blockchain::Monero => self.awaiting_funding_xmr.insert(swapid),
            Blockchain::Bitcoin => self.awaiting_funding_btc.insert(swapid),
        };
        if !newly_inserted {
            warn!(
                "{} | This swap was already in awaiting {} funding",
                swapid.swap_id(),
                blockchain.label()
            );
        }
    }

    pub fn incr_funded(&mut self, blockchain: &Blockchain, swapid: &SwapId) {
        let present_in_set = match blockchain {
            Blockchain::Monero => {
                self.funded_xmr += 1;
                self.awaiting_funding_xmr.remove(swapid)
            }
            Blockchain::Bitcoin => {
                self.funded_btc += 1;
                self.awaiting_funding_btc.remove(swapid)
            }
        };
        if !present_in_set {
            warn!(
                "{} | This swap wasn't awaiting {} funding",
                swapid.swap_id(),
                blockchain.label()
            );
        }
    }

    pub fn incr_funding_canceled(&mut self, blockchain: &Blockchain, swapid: &SwapId) {
        let present_in_set = match blockchain {
            Blockchain::Monero => {
                let presence = self.awaiting_funding_xmr.remove(swapid);
                self.funding_canceled_xmr += 1;
                presence
            }
            Blockchain::Bitcoin => {
                let presence = self.awaiting_funding_btc.remove(swapid);
                self.funding_canceled_btc += 1;
                presence
            }
        };
        if !present_in_set {
            warn!(
                "{} | This swap wasn't awaiting {} funding",
                swapid.swap_id(),
                blockchain.label()
            );
        }
    }

    pub fn success_rate(&self) -> f64 {
        let Stats {
            success,
            refund,
            punish,
            abort,
            initialized,
            awaiting_funding_btc,
            awaiting_funding_xmr,
            funded_btc,
            funded_xmr,
            funding_canceled_xmr,
            funding_canceled_btc,
        } = self;
        let total = success + refund + punish + abort;
        let rate = *success as f64 / (total as f64);
        info!(
            "Swapped({}) | Refunded({}) / Punished({}) | Aborted({}) | Initialized({}) / AwaitingFundingXMR({}) / AwaitingFundingBTC({}) / FundedXMR({}) / FundedBTC({}) / FundingCanceledXMR({}) / FundingCanceledBTC({})",
            success.label(),
            refund.label(),
            punish.label(),
            abort.label(),
            initialized.label(),
            awaiting_funding_xmr.len().label(),
            awaiting_funding_btc.len().label(),
            funded_xmr.label(),
            funded_btc.label(),
            funding_canceled_xmr.label(),
            funding_canceled_btc.label(),
        );
        info!(
            "{} = {:>4.3}%",
            "Swap success".bright_blue_bold(),
            (rate * 100.).bright_yellow_bold(),
        );
        rate
    }
}
