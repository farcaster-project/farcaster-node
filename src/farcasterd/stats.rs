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
    success: u64,
    refund: u64,
    punish: u64,
    abort: u64,
    initialized: u64,
    awaiting_funding_btc: HashSet<SwapId>,
    awaiting_funding_xmr: HashSet<SwapId>,
    funded_xmr: u64,
    funded_btc: u64,
    funding_canceled_xmr: u64,
    funding_canceled_btc: u64,
}

impl Stats {
    pub fn incr_outcome(&mut self, outcome: &Outcome) {
        match outcome {
            Outcome::Buy => self.success += 1,
            Outcome::Refund => self.refund += 1,
            Outcome::Punish => self.punish += 1,
            Outcome::Abort => self.abort += 1,
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
                swapid.bright_blue_italic(),
                blockchain.bright_white_bold()
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
                swapid.bright_blue_italic(),
                "Bitcoin".bright_white_bold()
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
                swapid.bright_blue_italic(),
                "Bitcoin".bright_white_bold()
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
            success.bright_white_bold(),
            refund.bright_white_bold(),
            punish.bright_white_bold(),
            abort.bright_white_bold(),
            initialized,
            awaiting_funding_xmr.len().bright_white_bold(),
            awaiting_funding_btc.len().bright_white_bold(),
            funded_xmr.bright_white_bold(),
            funded_btc.bright_white_bold(),
            funding_canceled_xmr.bright_white_bold(),
            funding_canceled_btc.bright_white_bold(),
        );
        info!(
            "{} = {:>4.3}%",
            "Swap success".bright_blue_bold(),
            (rate * 100.).bright_yellow_bold(),
        );
        rate
    }
}
