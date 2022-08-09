use std::collections::HashMap;

use crate::{
    automata::Event,
    databased::checkpoint_send,
    rpc::{
        request::{self, BitcoinFundingInfo, FundingInfo, Msg},
        Failure, FailureCode, Request, ServiceBus,
    },
    swapd::{runtime::Runtime, CheckpointSwapd},
    syncerd::bitcoin_syncer::p2wpkh_signed_tx_fee,
    CtlServer, Endpoints, Error, LogStyle, ServiceId,
};
use farcaster_core::{
    role::{SwapRole, TradeRole},
    swap::btcxmr::message::CoreArbitratingSetup,
    transaction::TxLabel,
};
use microservices::esb::{self, Handler};

use super::{
    get_swap_id,
    runtime::{remote_params_candidate, PendingRequest, PendingRequests, PendingRequestsT},
    swap_state::{Awaiting, SwapPhase},
    State,
};

impl Runtime {
    /// Processes incoming RPC or peer requests updating state - and switching to a new state, if
    /// necessary. Returns bool indicating whether a successful state update happened
    pub fn process(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: Request,
    ) -> Result<bool, Error> {
        let event = Event::with(endpoints, self.identity(), source.clone(), request);
        let updated_state = match self.process_event(event) {
            Ok(_) => {
                // Ignoring possible reporting errors here and after: do not want to
                // halt the swap just because the client disconnected
                let _ = self.report_progress_message_to(
                    endpoints, source, "", // self.state().state_machine.info_message(swap_id),
                );
                true
            }
            // We pass ESB errors forward such that they can fail the swap.
            // In the future they can be caught here and used to re-iterate sending of the same
            // message later without swap halting.
            Err(err @ Error::Esb(_)) => {
                error!(
                    "{} due to ESB failure: {}",
                    "Failing swap".err(),
                    err.err_details()
                );
                self.report_failure_to(
                    endpoints,
                    self.enquirer(),
                    Failure {
                        code: FailureCode::Unknown, // FIXME
                        info: err.to_string(),
                    },
                );
                return Err(err);
            }
            Err(other_err) => {
                error!("{}: {}", "Swap error".err(), other_err.err_details());
                self.report_failure_to(
                    endpoints,
                    self.enquirer(),
                    Failure {
                        code: FailureCode::Unknown,
                        info: other_err.to_string(),
                    },
                );
                false
            }
        };
        // if updated_state {
        //     self.save_state()?;
        //     info!(
        //         "ChannelStateMachine {} switched to {} state",
        //         self.state().channel.active_channel_id(),
        //         self.state().state_machine
        //     );
        // }
        Ok(updated_state)
    }

    fn process_event(&mut self, event: Event<Request>) -> Result<(), Error> {
        match Awaiting::with(self.state(), &event, Some(self.syncer_state())) {
            // Msg bus
            Awaiting::MakerCommit => self.process_maker_commit(event),
            Awaiting::RevealProof => self.process_reveal_proof(event),
            Awaiting::RevealAlice => self.process_reveal_alice(event),
            Awaiting::Reveal => self.process_reveal(event),
            Awaiting::CoreArb => self.process_core_arb(event),
            Awaiting::RefundSigs => self.process_refund_sigs(event),
            Awaiting::BuySig => self.process_buy_sig(event),
            // Ctl bus

            // Rest
            Awaiting::Unknown => Err(Error::Farcaster(s!("Invalid state for this event"))),
        }
    }

    pub fn process_event_maker(&mut self, event: Event<Request>) -> Result<(), Error> {
        todo!()
    }

    pub fn process_event_alice(&self, event: Event<Request>) -> Result<(), Error> {
        todo!()
    }

    pub fn process_event_bob(&self, event: Event<Request>) -> Result<(), Error> {
        todo!()
    }
}

/// Ctl bus processing fns
impl Runtime {}

/// Msg bus processing fns
impl Runtime {
    fn process_reveal(&mut self, mut event: Event<Request>) -> Result<(), Error> {
        // bob and alice
        // store parameters from counterparty if we have not received them yet.
        // if we're maker, also reveal to taker if their commitment is valid.

        if let Request::Protocol(Msg::Reveal(ref reveal)) = event.message {
            let remote_commit = self.state().remote_commit().cloned().unwrap();

            if let Ok(remote_params_candidate) = remote_params_candidate(reveal, remote_commit) {
                debug!("{:?} sets remote_params", self.state().swap_role());
                self.state.sup_remote_params(remote_params_candidate);
            } else {
                error!("Revealed remote params not preimage of commitment");
            }

            // Specific to swap roles
            // pass request on to wallet daemon so that it can set remote params
            match self.state().swap_role() {
                // validated state above, no need to check again
                SwapRole::Alice => {
                    // Alice already sends RevealProof immediately, so only have to
                    // forward Reveal now
                    event.forward(ServiceBus::Msg, ServiceId::Wallet)?
                }
                SwapRole::Bob
                    if self.syncer_state().btc_fee_estimate_sat_per_kvb.is_some()
                        && self.state().b_address().is_some() =>
                {
                    let address = self.state().b_address().cloned().unwrap();
                    let sat_per_kvb = self.syncer_state().btc_fee_estimate_sat_per_kvb.unwrap();
                    self.ask_bob_to_fund(sat_per_kvb, address, event.endpoints)?;

                    // sending this request will initialize the
                    // arbitrating setup, that can be only performed
                    // after the funding tx was seen
                    let pending_req = PendingRequest::from_event(&event, ServiceBus::Msg);
                    self.pending_requests()
                        .defer_request(ServiceId::Wallet, pending_req);
                }
                _ => unreachable!(
                    "Bob btc_fee_estimate_sat_per_kvb.is_none() was handled previously"
                ),
            }

            // up to here for both maker and taker, following only Maker

            // if did not yet reveal, maker only. on the msg flow as
            // of 2021-07-13 taker reveals first
            if self.state().commit() && self.state().trade_role() == Some(TradeRole::Maker) {
                if let Some(addr) = self.state().b_address().cloned() {
                    let txlabel = TxLabel::Funding;
                    if !self.syncer_state().is_watched_addr(&txlabel) {
                        let watch_addr_task = self.syncer_state.watch_addr_btc(addr, txlabel);
                        let btc_syncer = self.syncer_state().bitcoin_syncer();
                        event.complete_ctl_service(
                            btc_syncer,
                            Request::SyncerTask(watch_addr_task),
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    fn process_reveal_proof(&mut self, mut event: Event<Request>) -> Result<(), Error> {
        // These messages are saved as pending if Bob and then forwarded once the
        // parameter reveal forward is triggered. If Alice, send immediately.
        match self.state().swap_role() {
            SwapRole::Bob => {
                let pending_request = PendingRequest::from_event(&event, ServiceBus::Msg);
                self.pending_requests()
                    .defer_request(ServiceId::Wallet, pending_request);
            }
            SwapRole::Alice => {
                event.forward(ServiceBus::Msg, ServiceId::Wallet)?;
            }
        }
        Ok(())
    }

    fn process_reveal_alice(&mut self, event: Event<Request>) -> Result<(), Error> {
        if self.state().b_address().is_none() {
            let msg = format!("FIXME: b_address is None, request {}", event.message);
            error!("{}", msg);
            return Err(Error::Farcaster(msg));
        }
        debug!(
            "Deferring request {} for when preconditions satisfied, then recurse in the runtime",
            &event.message
        );
        let pending_req = PendingRequest::from_event(&event, ServiceBus::Msg);
        let btc_syncer = self.syncer_state().bitcoin_syncer();
        self.pending_requests()
            .defer_request(btc_syncer, pending_req);
        Ok(())
    }

    fn process_maker_commit(&mut self, mut event: Event<Request>) -> Result<(), Error> {
        if let Request::Protocol(Msg::MakerCommit(ref remote_commit)) = event.message {
            trace!("received remote commitment");
            self.state.t_sup_remote_commit(remote_commit.clone());
            if self.state().swap_role() == SwapRole::Bob {
                let addr = self
                    .state()
                    .b_address()
                    .cloned()
                    .expect("address available at CommitB");
                let txlabel = TxLabel::Funding;
                if !self.syncer_state().is_watched_addr(&txlabel) {
                    let task = self.syncer_state.watch_addr_btc(addr, txlabel);
                    self.send_ctl(
                        event.endpoints,
                        self.syncer_state().bitcoin_syncer(),
                        Request::SyncerTask(task),
                    )?;
                }
            }
            event.forward(ServiceBus::Msg, ServiceId::Wallet)?;
        };
        Ok(())
    }

    fn process_core_arb(&mut self, mut event: Event<Request>) -> Result<(), Error> {
        // alice receives, bob sends
        if let Request::Protocol(Msg::CoreArbitratingSetup(CoreArbitratingSetup {
            lock,
            cancel,
            refund,
            ..
        })) = event.message.clone()
        {
            for (tx, tx_label) in
                [lock, cancel, refund]
                    .iter()
                    .zip([TxLabel::Lock, TxLabel::Cancel, TxLabel::Refund])
            {
                let tx = (&*tx).clone().extract_tx();
                let txid = tx.txid();
                debug!(
                    "tx_label: {}, vsize: {}, outs: {}",
                    tx_label,
                    tx.vsize(),
                    tx.output.len()
                );
                if !self.syncer_state().is_watched_tx(&tx_label) {
                    let task = self.syncer_state.watch_tx_btc(txid, tx_label);
                    event.send_ctl_service(
                        self.syncer_state().bitcoin_syncer(),
                        Request::SyncerTask(task),
                    )?;
                }
                if tx_label == TxLabel::Refund {
                    self.syncer_state.tasks.txids.insert(TxLabel::Refund, txid);
                }
            }
            event.forward(ServiceBus::Msg, ServiceId::Wallet)?;
        }
        Ok(())
    }

    fn process_refund_sigs(&mut self, mut event: Event<Request>) -> Result<(), Error> {
        self.state.sup_received_refund_procedure_signatures();
        event.forward(ServiceBus::Msg, ServiceId::Wallet)?;
        Ok(())
    }

    fn process_buy_sig(&mut self, mut event: Event<Request>) -> Result<(), Error> {
        // alice receives, bob sends
        if let Request::Protocol(Msg::BuyProcedureSignature(buy_proc_sig)) = event.message.clone() {
            // Alice verifies that she has sent refund procedure signatures before
            // processing the buy signatures from Bob
            let tx_label = TxLabel::Buy;
            if !self.syncer_state().is_watched_tx(&tx_label) {
                let txid = buy_proc_sig.buy.clone().extract_tx().txid();
                let task = self.syncer_state.watch_tx_btc(txid, tx_label);
                event.send_ctl_service(
                    self.syncer_state().bitcoin_syncer(),
                    Request::SyncerTask(task),
                )?;
            }

            // checkpoint swap alice pre buy
            debug!(
                "{} | checkpointing alice pre buy swapd state",
                self.swap_id().bright_blue_italic()
            );
            if self.state.a_sup_checkpoint_pre_buy() {
                checkpoint_send(
                    event.endpoints,
                    self.swap_id(),
                    self.identity(),
                    ServiceId::Database,
                    request::CheckpointState::CheckpointSwapd(CheckpointSwapd {
                        state: self.state().clone(),
                        last_msg: Msg::BuyProcedureSignature(buy_proc_sig.clone()),
                        enquirer: self.enquirer(),
                        temporal_safety: self.temporal_safety.clone(),
                        txs: self.txs.clone(),
                        txids: self.syncer_state().tasks.txids.clone(),
                        pending_requests: self.pending_requests().clone(),
                        pending_broadcasts: self.syncer_state().pending_broadcast_txs(),
                        xmr_addr_addendum: self.syncer_state().xmr_addr_addendum.clone(),
                    }),
                )?;
            }
            event.forward(ServiceBus::Msg, ServiceId::Wallet)?;
        }
        Ok(())
    }

    fn ask_bob_to_fund(
        &mut self,
        sat_per_kvb: u64,
        address: bitcoin::Address,
        endpoints: &mut Endpoints,
    ) -> Result<(), Error> {
        let swap_id = self.swap_id();
        let vsize = 94;
        let nr_inputs = 1;
        let total_fees =
            bitcoin::Amount::from_sat(p2wpkh_signed_tx_fee(sat_per_kvb, vsize, nr_inputs));
        let amount = self.syncer_state.bitcoin_amount + total_fees;
        info!(
            "{} | Send {} to {}, this includes {} for the Lock transaction network fees",
            swap_id.bright_blue_italic(),
            amount.bright_green_bold(),
            address.addr(),
            total_fees
        );
        self.state.b_sup_required_funding_amount(amount);
        let req = Request::FundingInfo(FundingInfo::Bitcoin(BitcoinFundingInfo {
            swap_id,
            address,
            amount,
        }));
        self.syncer_state.awaiting_funding = true;

        if let Some(enquirer) = self.enquirer() {
            endpoints.send_to(ServiceBus::Ctl, self.identity(), enquirer, req)?;
        }
        Ok(())
    }
}
