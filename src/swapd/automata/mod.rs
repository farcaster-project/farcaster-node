use std::{
    collections::HashMap,
    time::{Duration, SystemTime},
};

use crate::{
    automata::Event,
    databased::checkpoint_send,
    rpc::{
        request::{
            self, BitcoinFundingInfo, CheckpointState, FundingInfo, InitSwap, Msg, Outcome, Params,
            Reveal, Tx,
        },
        Failure, FailureCode, Request, ServiceBus,
    },
    swapd::{
        runtime::Runtime,
        swap_state::{AliceState, BobState},
        CheckpointSwapd, SwapCheckpointType,
    },
    syncerd::{bitcoin_syncer::p2wpkh_signed_tx_fee, SweepMoneroAddress, XmrAddressAddendum},
    CtlServer, Endpoints, Error, LogStyle, ServiceId,
};
use farcaster_core::{
    blockchain::Blockchain,
    role::{SwapRole, TradeRole},
    swap::btcxmr::message::CoreArbitratingSetup,
    transaction::TxLabel,
};
use microservices::esb::{self, Handler};

use super::{
    get_swap_id,
    runtime::{
        aggregate_xmr_spend_view, remote_params_candidate, PendingRequest, PendingRequests,
        PendingRequestsT,
    },
    swap_state::Awaiting,
    syncer_client::log_tx_received,
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

                // FIXME: fix report messages
                // let _ = self.report_progress_message_to(
                //     endpoints,
                //     self.enquirer(),
                //     "", // self.state().state_machine.info_message(swap_id),
                // );

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
        match Awaiting::with(
            self.state(),
            &event,
            Some(self.syncer_state()),
            Some(&self.pending_requests),
        ) {
            // Msg bus
            Awaiting::MakerCommit => self.process_maker_commit(event),
            Awaiting::RevealProof => self.process_reveal_proof(event),
            Awaiting::RevealAlice => self.process_reveal_alice(event),
            Awaiting::Reveal => self.process_reveal(event),
            Awaiting::CoreArb => self.process_core_arb(event),
            Awaiting::RefundSigs => self.process_refund_sigs(event),
            Awaiting::BuySig => self.process_buy_sig(event),

            // Ctl bus
            Awaiting::TakeSwapCtl => self.take_swap_ctl(event),
            Awaiting::RevealProofCtl => self.process_reveal_proof_ctl(event),
            Awaiting::MakeSwapCtl => self.process_make_swap_ctl(event),
            Awaiting::FundingUpdatedCtl => self.process_funding_updated_ctl(event),
            Awaiting::CoreArbCtl => self.process_core_arb_ctl(event),
            Awaiting::RefundSigsCtl => self.process_refund_sigs_ctl(event),
            Awaiting::BuySigCtl => self.process_buy_sig_ctl(event),
            Awaiting::TxLock => self.process_tx_lock(event),
            Awaiting::Tx => self.process_tx(event),
            Awaiting::SweepMonero => self.process_sweep_monero(event),
            Awaiting::Terminate => self.process_terminate(),
            Awaiting::SweepBitcoin => self.process_sweep_bitcoin(event),
            Awaiting::PeerReconnected => self.peer_reconnected(event),
            Awaiting::Checkpoint => self.checkpoint(event),

            // RPC
            Awaiting::AbortSimple => self.process_abort_simple(event),
            Awaiting::AbortBob => self.process_abort_bob(event),
            Awaiting::AbortBlocked => self.process_abort_blocked(event),
            Awaiting::GetInfo => self.get_info(event),

            // Syncer

            // Others
            Awaiting::Unknown => Err(Error::Farcaster(s!("Invalid state for this event"))),
        }
    }
}

/// Ctl bus processing fns
impl Runtime {
    fn take_swap_ctl(&mut self, event: Event<Request>) -> Result<(), Error> {
        if let Request::TakeSwap(InitSwap {
            peerd,
            report_to,
            local_params,
            swap_id,
            remote_commit: None,
            funding_address, // Some(_) for Bob, None for Alice
        }) = event.message
        {
            self.syncer_state.watch_fee_and_height(event.endpoints)?;

            self.peer_service = peerd.clone();
            self.enquirer = report_to.clone();

            if let ServiceId::Peer(ref addr) = peerd {
                self.maker_peer = Some(addr.clone());
            }
            let local_commit = self.taker_commit(event.endpoints, local_params.clone())?;
            let next_state = self.state.clone().sup_start_to_commit(
                local_commit.clone(),
                local_params,
                funding_address,
                None,
            );
            let take_swap = request::TakeCommit {
                commit: local_commit,
                public_offer: self.public_offer.to_string(),
                swap_id,
            };
            self.send_peer(event.endpoints, Msg::TakerCommit(take_swap))?;
            self.state_update(event.endpoints, next_state)?;
        }
        Ok(())
    }

    fn process_reveal_proof_ctl(&mut self, event: Event<Request>) -> Result<(), Error> {
        if let Request::Protocol(Msg::Reveal(request::Reveal::Proof(proof))) = event.message.clone()
        {
            let reveal_proof = Msg::Reveal(request::Reveal::Proof(proof));
            let swap_id = reveal_proof.swap_id();
            self.send_peer(event.endpoints, reveal_proof)?;
            trace!("sent reveal_proof to peerd");
            let local_params = self
                .state
                .local_params()
                .expect("commit state has local_params");
            let reveal_params: Reveal = (swap_id, local_params.clone()).into();
            self.send_peer(event.endpoints, Msg::Reveal(reveal_params))?;
            trace!("sent reveal_proof to peerd");
            let next_state = self.state.clone().sup_commit_to_reveal();
            self.state_update(event.endpoints, next_state)?;
        }
        Ok(())
    }
    fn process_make_swap_ctl(&mut self, event: Event<Request>) -> Result<(), Error> {
        if let Request::MakeSwap(InitSwap {
            peerd,
            report_to,
            local_params,
            swap_id,
            remote_commit: Some(remote_commit),
            funding_address, // Some(_) for Bob, None for Alice
        }) = event.message.clone()
        {
            self.syncer_state.watch_fee_and_height(event.endpoints)?;
            self.peer_service = peerd.clone();
            if let ServiceId::Peer(ref addr) = peerd {
                self.maker_peer = Some(addr.clone());
            }
            self.enquirer = report_to.clone();
            let local_commit =
                self.maker_commit(event.endpoints, &peerd, swap_id, &local_params)?;
            let next_state = self.state.clone().sup_start_to_commit(
                local_commit.clone(),
                local_params,
                funding_address,
                Some(remote_commit),
            );

            trace!("sending peer MakerCommit msg {}", &local_commit);
            self.send_peer(event.endpoints, Msg::MakerCommit(local_commit))?;
            self.state_update(event.endpoints, next_state)?;
        }
        Ok(())
    }

    fn process_funding_updated_ctl(&mut self, event: Event<Request>) -> Result<(), Error> {
        let success_proof = PendingRequests::continue_deferred_requests(
            self,
            event.endpoints,
            event.source.clone(),
            |r| {
                matches!(
                    r,
                    &PendingRequest {
                        dest: ServiceId::Wallet,
                        bus_id: ServiceBus::Msg,
                        request: Request::Protocol(Msg::Reveal(Reveal::Proof(_))),
                        ..
                    }
                )
            },
        );
        if !success_proof {
            error!("Did not dispatch proof pending request");
        }

        let success_params =
            PendingRequests::continue_deferred_requests(self, event.endpoints, event.source, |r| {
                matches!(
                    r,
                    &PendingRequest {
                        dest: ServiceId::Wallet,
                        bus_id: ServiceBus::Msg,
                        request: Request::Protocol(Msg::Reveal(Reveal::AliceParameters(_))),
                        ..
                    }
                )
            });
        if !success_params {
            error!("Did not dispatch params pending requests");
        }
        Ok(())
    }

    fn process_core_arb_ctl(&mut self, event: Event<Request>) -> Result<(), Error> {
        if let Request::Protocol(Msg::CoreArbitratingSetup(core_arb_setup)) = event.message {
            // checkpoint swap pre lock bob
            debug!(
                "{} | checkpointing bob pre lock swapd state",
                self.swap_id().bright_blue_italic()
            );
            if self.state.b_sup_checkpoint_pre_lock() {
                checkpoint_send(
                    event.endpoints,
                    self.swap_id(),
                    self.identity(),
                    ServiceId::Database,
                    request::CheckpointState::CheckpointSwapd(CheckpointSwapd {
                        state: self.state.clone(),
                        last_msg: Msg::CoreArbitratingSetup(core_arb_setup.clone()),
                        enquirer: self.enquirer.clone(),
                        temporal_safety: self.temporal_safety.clone(),
                        txs: self.txs.clone(),
                        txids: self.syncer_state.tasks.txids.clone(),
                        pending_requests: self.pending_requests().clone(),
                        pending_broadcasts: self.syncer_state.pending_broadcast_txs(),
                        xmr_addr_addendum: self.syncer_state.xmr_addr_addendum.clone(),
                    }),
                )?;
            }
            let CoreArbitratingSetup {
                swap_id: _,
                lock,
                cancel,
                refund,
                cancel_sig: _,
            } = core_arb_setup.clone();
            for (tx, tx_label) in
                [lock, cancel, refund]
                    .iter()
                    .zip([TxLabel::Lock, TxLabel::Cancel, TxLabel::Refund])
            {
                if !self.syncer_state.is_watched_tx(&tx_label) {
                    let txid = tx.clone().extract_tx().txid();
                    let task = self.syncer_state.watch_tx_btc(txid, tx_label);
                    event.endpoints.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        self.syncer_state.bitcoin_syncer(),
                        Request::SyncerTask(task),
                    )?;
                }
            }
            trace!("sending peer CoreArbitratingSetup msg: {}", &core_arb_setup);
            self.send_peer(event.endpoints, Msg::CoreArbitratingSetup(core_arb_setup))?;
            let next_state = State::Bob(BobState::CorearbB {
                received_refund_procedure_signatures: false,
                local_params: self.state.local_params().cloned().unwrap(),
                cancel_seen: false,
                remote_params: self.state.remote_params().unwrap(),
                b_address: self.state.b_address().cloned().unwrap(),
                last_checkpoint_type: self.state.last_checkpoint_type().unwrap(),
            });
            self.state_update(event.endpoints, next_state)?;
        }
        Ok(())
    }

    fn process_refund_sigs_ctl(&mut self, event: Event<Request>) -> Result<(), Error> {
        if let Request::Protocol(Msg::RefundProcedureSignatures(refund_proc_sigs)) = event.message {
            // checkpoint alice pre lock bob
            debug!(
                "{} | checkpointing alice pre lock swapd state",
                self.swap_id().bright_blue_italic()
            );
            if self.state.a_sup_checkpoint_pre_lock() {
                checkpoint_send(
                    event.endpoints,
                    self.swap_id(),
                    self.identity(),
                    ServiceId::Database,
                    request::CheckpointState::CheckpointSwapd(CheckpointSwapd {
                        state: self.state.clone(),
                        last_msg: Msg::RefundProcedureSignatures(refund_proc_sigs.clone()),
                        enquirer: self.enquirer.clone(),
                        temporal_safety: self.temporal_safety.clone(),
                        txs: self.txs.clone(),
                        txids: self.syncer_state.tasks.txids.clone(),
                        pending_requests: self.pending_requests().clone(),
                        pending_broadcasts: self.syncer_state.pending_broadcast_txs(),
                        xmr_addr_addendum: self.syncer_state.xmr_addr_addendum.clone(),
                    }),
                )?;
            }

            self.send_peer(
                event.endpoints,
                Msg::RefundProcedureSignatures(refund_proc_sigs),
            )?;
            trace!("sent peer RefundProcedureSignatures msg");
            let next_state = State::Alice(AliceState::RefundSigA {
                last_checkpoint_type: SwapCheckpointType::CheckpointAlicePreLock,
                local_params: self.state.local_params().cloned().unwrap(),
                btc_locked: false,
                xmr_locked: false,
                buy_published: false,
                cancel_seen: false,
                refund_seen: false,
                remote_params: self.state.remote_params().unwrap(),
                required_funding_amount: None,
                overfunded: false,
            });
            self.state_update(event.endpoints, next_state)?;
        }
        Ok(())
    }

    fn process_buy_sig_ctl(&mut self, event: Event<Request>) -> Result<(), Error> {
        if let Request::Protocol(Msg::BuyProcedureSignature(ref buy_proc_sig)) = event.message {
            // checkpoint bob pre buy
            debug!(
                "{} | checkpointing bob pre buy swapd state",
                self.swap_id().bright_blue_italic()
            );
            if self.state.b_sup_checkpoint_pre_buy() {
                checkpoint_send(
                    event.endpoints,
                    self.swap_id(),
                    self.identity(),
                    ServiceId::Database,
                    request::CheckpointState::CheckpointSwapd(CheckpointSwapd {
                        state: self.state.clone(),
                        last_msg: Msg::BuyProcedureSignature(buy_proc_sig.clone()),
                        enquirer: self.enquirer.clone(),
                        temporal_safety: self.temporal_safety.clone(),
                        txs: self.txs.clone(),
                        txids: self.syncer_state.tasks.txids.clone(),
                        pending_requests: self.pending_requests().clone(),
                        pending_broadcasts: self.syncer_state.pending_broadcast_txs(),
                        xmr_addr_addendum: self.syncer_state.xmr_addr_addendum.clone(),
                    }),
                )?;
            }

            debug!("subscribing with syncer for receiving raw buy tx ");

            let buy_tx = buy_proc_sig.buy.clone().extract_tx();
            let txid = buy_tx.txid();
            // register Buy tx task
            let tx_label = TxLabel::Buy;
            if !self.syncer_state.is_watched_tx(&tx_label) {
                let task = self.syncer_state.watch_tx_btc(txid, tx_label);
                event.endpoints.send_to(
                    ServiceBus::Ctl,
                    self.identity(),
                    self.syncer_state.bitcoin_syncer(),
                    Request::SyncerTask(task),
                )?;
            }
            // set external eddress: needed to subscribe for buy tx (bob) or refund (alice)
            self.syncer_state.tasks.txids.insert(TxLabel::Buy, txid);
            let pending_request = PendingRequest::from_event_forward(
                &event,
                ServiceBus::Msg,
                self.peer_service.clone(),
            );
            self.pending_requests
                .defer_request(self.syncer_state.monero_syncer(), pending_request);
        }
        Ok(())
    }

    fn process_tx_lock(&mut self, event: Event<Request>) -> Result<(), Error> {
        if let Request::Tx(Tx::Lock(btc_lock)) = event.message {
            log_tx_received(self.swap_id(), TxLabel::Lock);
            self.broadcast(btc_lock, TxLabel::Lock, event.endpoints)?;
            if let (Some(Params::Bob(bob_params)), Some(Params::Alice(alice_params))) =
                (&self.state.local_params(), &self.state.remote_params())
            {
                let (spend, view) = aggregate_xmr_spend_view(alice_params, bob_params);

                let txlabel = TxLabel::AccLock;
                if !self.syncer_state.is_watched_addr(&txlabel) {
                    let task = self.syncer_state.watch_addr_xmr(spend, view, txlabel, None);
                    event.endpoints.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        self.syncer_state.monero_syncer(),
                        Request::SyncerTask(task),
                    )?
                }
            } else {
                error!(
                    "local_params or remote_params not set, state {}",
                    self.state
                )
            };
        }
        Ok(())
    }

    fn process_tx(&mut self, event: Event<Request>) -> Result<(), Error> {
        if let Request::Tx(transaction) = event.message {
            // update state
            match transaction.clone() {
                Tx::Cancel(tx) => {
                    log_tx_received(self.swap_id(), TxLabel::Cancel);
                    self.txs.insert(TxLabel::Cancel, tx);
                }
                Tx::Refund(tx) => {
                    log_tx_received(self.swap_id(), TxLabel::Refund);
                    self.txs.insert(TxLabel::Refund, tx);
                }
                Tx::Punish(tx) => {
                    log_tx_received(self.swap_id(), TxLabel::Punish);
                    self.txs.insert(TxLabel::Punish, tx);
                }
                Tx::Buy(tx) => {
                    log_tx_received(self.swap_id(), TxLabel::Buy);
                    self.txs.insert(TxLabel::Buy, tx);
                }
                Tx::Funding(_) => unreachable!("not handled in swapd"),
                Tx::Lock(_) => unreachable!("handled above"),
            }
            // replay last tx confirmation event received from syncer, recursing
            let source = self.syncer_state.bitcoin_syncer();
            match transaction {
                Tx::Cancel(_) | Tx::Buy(_) => {
                    // FIXME: use PendingRequests facility?
                    if let Some(lock_tx_confs_req) = self.syncer_state.lock_tx_confs.clone() {
                        // self.handle_ctl(endpoints, source, lock_tx_confs_req)?; // FIXME uncomment
                    }
                }
                Tx::Refund(_) | Tx::Punish(_) => {
                    if let Some(cancel_tx_confs_req) = self.syncer_state.cancel_tx_confs.clone() {
                        // self.handle_ctl(endpoints, source, cancel_tx_confs_req)?; // FIXME uncomment
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn process_sweep_monero(&mut self, event: Event<Request>) -> Result<(), Error> {
        if let Request::SweepMoneroAddress(SweepMoneroAddress {
            source_view_key,
            source_spend_key,
            destination_address,
            minimum_balance,
            ..
        }) = event.message
        {
            let from_height = None; // will be set when sending out the request
            let task = self.syncer_state.sweep_xmr(
                source_view_key,
                source_spend_key,
                destination_address,
                from_height,
                minimum_balance,
                true,
            );
            let acc_confs_needs =
                self.temporal_safety.sweep_monero_thr - self.temporal_safety.xmr_finality_thr;
            let sweep_block = self.syncer_state.height(Blockchain::Monero) + acc_confs_needs as u64;
            info!(
                "{} | Tx {} needs {}, and has {} {}",
                self.swap_id().bright_blue_italic(),
                TxLabel::AccLock.bright_white_bold(),
                "10 confirmations".bright_green_bold(),
                (10 - acc_confs_needs).bright_green_bold(),
                "confirmations".bright_green_bold(),
            );
            info!(
                "{} | {} reaches your address {} around block {}",
                self.swap_id().bright_blue_italic(),
                Blockchain::Monero.bright_white_bold(),
                destination_address.bright_yellow_bold(),
                sweep_block.bright_blue_bold(),
            );
            warn!(
                "Peerd might crash, just ignore it, counterparty closed\
                   connection but you don't need it anymore!"
            );
            let request = Request::SyncerTask(task);
            let dest = self.syncer_state.monero_syncer();
            let pending_request =
                PendingRequest::new(self.identity(), dest.clone(), ServiceBus::Ctl, request);
            self.pending_requests.defer_request(dest, pending_request);
        }
        Ok(())
    }
    fn process_terminate(&mut self) -> Result<(), Error> {
        info!(
            "{} | {}",
            self.swap_id().bright_blue_italic(),
            format!("Terminating {}", self.identity()).bright_white_bold()
        );
        std::process::exit(0);
    }
    fn process_sweep_bitcoin(&mut self, event: Event<Request>) -> Result<(), Error> {
        if let Request::SweepBitcoinAddress(sweep_bitcoin_address) = event.message.clone() {
            info!(
                "{} | Sweeping source (funding) address: {} to destination address: {}",
                self.swap_id(),
                sweep_bitcoin_address.source_address,
                sweep_bitcoin_address.destination_address
            );

            let task = self.syncer_state.sweep_btc(sweep_bitcoin_address, false);
            event.complete_ctl_service(
                self.syncer_state.bitcoin_syncer(),
                Request::SyncerTask(task),
            )?;
        }
        Ok(())
    }
    fn process_abort_simple(&mut self, event: Event<Request>) -> Result<(), Error> {
        // just cancel the swap, no additional logic required
        let new_state = match self.state.swap_role() {
            SwapRole::Alice => State::Alice(AliceState::FinishA(Outcome::Abort)),
            SwapRole::Bob => State::Bob(BobState::FinishB(Outcome::Abort)),
        };
        self.state_update(event.endpoints, new_state)?;
        self.abort_swap(event.endpoints)?;
        if let ServiceId::Client(_) = event.source {
            self.send_ctl(
                event.endpoints,
                event.source,
                Request::String("Aborted swap".to_string()),
            )?;
        }
        Ok(())
    }

    fn process_abort_bob(&mut self, event: Event<Request>) -> Result<(), Error> {
        self.send_ctl(
            event.endpoints,
            ServiceId::Wallet,
            Request::GetSweepBitcoinAddress(self.state.b_address().cloned().unwrap()),
        )?;
        // cancel the swap to invalidate its state
        self.state_update(
            event.endpoints,
            State::Bob(BobState::FinishB(Outcome::Abort)),
        )?;
        if let ServiceId::Client(_) = event.source {
            self.send_ctl(
                event.endpoints,
                event.source,
                Request::String("Aborting swap, checking if funds can be sweeped.".to_string()),
            )?;
        }
        Ok(())
    }
    fn process_abort_blocked(&mut self, event: Event<Request>) -> Result<(), Error> {
        let msg = "Swap is already locked-in, cannot manually abort anymore.".to_string();
        warn!("{} | {}", self.swap_id(), msg);

        if let ServiceId::Client(_) = event.source {
            self.send_ctl(
                event.endpoints,
                event.source,
                Request::Failure(Failure {
                    code: FailureCode::Unknown,
                    info: msg,
                }),
            )?;
        }
        Ok(())
    }

    fn get_info(&mut self, event: Event<Request>) -> Result<(), Error> {
        {
            let swap_id = if self.swap_id() == zero!() {
                None
            } else {
                Some(self.swap_id())
            };
            let info = request::SwapInfo {
                swap_id,
                // state: self.state, // FIXME serde missing
                maker_peer: self.maker_peer.clone().map(|p| vec![p]).unwrap_or_default(),
                uptime: SystemTime::now()
                    .duration_since(self.started)
                    .unwrap_or_else(|_| Duration::from_secs(0)),
                since: self
                    .started
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_else(|_| Duration::from_secs(0))
                    .as_secs(),
                public_offer: self.public_offer.clone(),
            };
            event.complete_ctl(Request::SwapInfo(info))?;
            Ok(())
        }
    }
    fn peer_reconnected(&mut self, event: Event<Request>) -> Result<(), Error> {
        for msg in self.pending_peer_request.clone().iter() {
            self.send_peer(event.endpoints, msg.clone())?;
        }
        self.pending_peer_request.clear();
        Ok(())
    }

    fn checkpoint(&mut self, event: Event<Request>) -> Result<(), Error> {
        if let Request::Checkpoint(request::Checkpoint { swap_id, state }) = event.message {
            match state {
                CheckpointState::CheckpointSwapd(CheckpointSwapd {
                    state,
                    last_msg,
                    enquirer,
                    temporal_safety,
                    txs,
                    txids,
                    pending_requests,
                    pending_broadcasts,
                    xmr_addr_addendum,
                }) => {
                    info!("{} | Restoring swap", swap_id);
                    self.state = state;
                    self.enquirer = enquirer;
                    self.temporal_safety = temporal_safety;
                    self.pending_requests = pending_requests;
                    self.txs = txs.clone();
                    trace!("Watch height bitcoin");
                    let watch_height_bitcoin = self.syncer_state.watch_height(Blockchain::Bitcoin);
                    event.endpoints.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        self.syncer_state.bitcoin_syncer(),
                        Request::SyncerTask(watch_height_bitcoin),
                    )?;

                    trace!("Watch height monero");
                    let watch_height_monero = self.syncer_state.watch_height(Blockchain::Monero);
                    event.endpoints.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        self.syncer_state.monero_syncer(),
                        Request::SyncerTask(watch_height_monero),
                    )?;

                    trace!("Watching transactions");
                    for (tx_label, txid) in txids.iter() {
                        let task = self
                            .syncer_state
                            .watch_tx_btc(txid.clone(), tx_label.clone());
                        event.endpoints.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            self.syncer_state.bitcoin_syncer(),
                            Request::SyncerTask(task),
                        )?;
                    }

                    trace!("broadcasting txs pending broadcast");
                    for tx in pending_broadcasts.iter() {
                        let task = self.syncer_state.broadcast(tx.clone());
                        event.endpoints.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            self.syncer_state.bitcoin_syncer(),
                            Request::SyncerTask(task),
                        )?;
                    }

                    if let Some(XmrAddressAddendum {
                        view_key,
                        spend_key,
                        from_height,
                    }) = xmr_addr_addendum
                    {
                        let task = self.syncer_state.watch_addr_xmr(
                            spend_key,
                            view_key,
                            TxLabel::AccLock,
                            Some(from_height),
                        );
                        event.endpoints.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            self.syncer_state.monero_syncer(),
                            Request::SyncerTask(task),
                        )?;
                    }
                    let msg = format!("Restored swap at state {}", self.state);
                    let _ = self.report_progress_message_to(
                        event.endpoints,
                        ServiceId::Farcasterd,
                        msg,
                    );

                    self.handle_ctl(
                        event.endpoints,
                        ServiceId::Database,
                        Request::Protocol(last_msg),
                    )?;
                }
                s => {
                    error!("Checkpoint {} not supported in swapd", s);
                }
            }
        }
        Ok(())
    }

    // match request {
    //     Request::SyncerEvent(ref event) if source == self.syncer_state.bitcoin_syncer => {
    //         match &event {
    //             Event::HeightChanged(HeightChanged { height, .. }) => {
    //                 self.syncer_state
    //                     .handle_height_change(*height, Blockchain::Bitcoin);
    //             }
    //             Event::AddressTransaction(AddressTransaction { id, amount, tx, .. })
    //                 if self.syncer_state.tasks.watched_addrs.get(id).is_some() =>
    //             {
    //                 let tx = bitcoin::Transaction::deserialize(tx)?;
    //                 info!(
    //                     "Received AddressTransaction, processing tx {}",
    //                     &tx.txid().addr()
    //                 );
    //                 let txlabel = self.syncer_state.tasks.watched_addrs.get(id).unwrap();
    //                 match txlabel {
    //                     TxLabel::Funding
    //                         if self.syncer_state.awaiting_funding
    //                             && self.state.b_required_funding_amount().is_some() =>
    //                     {
    //                         log_tx_seen(self.swap_id, txlabel, &tx.txid());
    //                         self.syncer_state.awaiting_funding = false;
    //                         endpoints.send_to(
    //                             ServiceBus::Ctl,
    //                             self.identity(),
    //                             ServiceId::Farcasterd,
    //                             Request::FundingCompleted(Blockchain::Bitcoin),
    //                         )?;
    //                         // If the bitcoin amount does not match the expected funding amount, abort the swap
    //                         let amount = bitcoin::Amount::from_sat(*amount);
    //                         info!(
    //                             "LMAO: {}, {:?}",
    //                             self.state,
    //                             self.state.b_required_funding_amount()
    //                         );
    //                         let required_funding_amount =
    //                             self.state.b_required_funding_amount().unwrap();

    //                         if amount != required_funding_amount {
    //                             let msg = format!("Incorrect amount funded. Required: {}, Funded: {}. Do not fund this swap anymore, will abort and atttempt to sweep the Bitcoin to the provided address.", amount, required_funding_amount);
    //                             error!("{}", msg);
    //                             self.report_progress_message_to(
    //                                 endpoints,
    //                                 ServiceId::Farcasterd,
    //                                 msg,
    //                             )?;
    //                             // FIXME: syncer shall not have permission to AbortSwap, replace source by identity?
    //                             self.handle_ctl(endpoints, source, Request::AbortSwap)?;
    //                             return Ok(());
    //                         }

    //                         let req = Request::Tx(Tx::Funding(tx));
    //                         self.send_wallet(ServiceBus::Ctl, endpoints, req)?;
    //                     }

    //                     txlabel => {
    //                         error!(
    //                             "address transaction event not supported for tx {} at state {}",
    //                             txlabel, &self.state
    //                         )
    //                     }
    //                 }
    //             }
    //             Event::AddressTransaction(AddressTransaction { tx, .. }) => {
    //                 let tx = bitcoin::Transaction::deserialize(tx)?;
    //                 warn!(
    //                     "unknown address transaction with txid {}",
    //                     &tx.txid().addr()
    //                 )
    //             }
    //             Event::TransactionRetrieved(TransactionRetrieved { id, tx: Some(tx) })
    //                 if self.syncer_state.tasks.retrieving_txs.contains_key(id) =>
    //             {
    //                 let (txlabel, _) =
    //                     self.syncer_state.tasks.retrieving_txs.remove(id).unwrap();
    //                 match txlabel {
    //                     TxLabel::Buy if self.state.b_buy_sig() => {
    //                         log_tx_seen(self.swap_id, &txlabel, &tx.txid());
    //                         self.state.b_sup_buysig_buy_tx_seen();
    //                         let req = Request::Tx(Tx::Buy(tx.clone()));
    //                         self.send_wallet(ServiceBus::Ctl, endpoints, req)?
    //                     }
    //                     TxLabel::Buy => {
    //                         warn!(
    //                             "expected BobState(BuySigB), found {}. Any chance you reused the \
    //                              destination/refund address in the cli command? For your own privacy, \
    //                              do not reuse bitcoin addresses. Txid {}",
    //                             self.state,
    //                             tx.txid().addr(),
    //                         )
    //                     }
    //                     TxLabel::Refund
    //                         if self.state.a_refundsig()
    //                             && self.state.a_xmr_locked()
    //                         // && !self.state.a_buy_published()
    //                         =>
    //                     {
    //                         log_tx_seen(self.swap_id, &txlabel, &tx.txid());
    //                         let req = Request::Tx(Tx::Refund(tx.clone()));
    //                         self.send_wallet(ServiceBus::Ctl, endpoints, req)?
    //                     }
    //                     txlabel => {
    //                         error!(
    //                             "Transaction retrieved event not supported for tx {} at state {}",
    //                             txlabel, &self.state
    //                         )
    //                     }
    //                 }
    //             }

    //             Event::TransactionRetrieved(TransactionRetrieved { id, tx: None })
    //                 if self.syncer_state.tasks.retrieving_txs.contains_key(id) =>
    //             {
    //                 let (_tx_label, task) =
    //                     self.syncer_state.tasks.retrieving_txs.get(id).unwrap();
    //                 std::thread::sleep(core::time::Duration::from_millis(500));
    //                 endpoints.send_to(
    //                     ServiceBus::Ctl,
    //                     self.identity(),
    //                     self.syncer_state.bitcoin_syncer(),
    //                     Request::SyncerTask(task.clone()),
    //                 )?;
    //             }
    //             Event::TransactionConfirmations(TransactionConfirmations {
    //                 id,
    //                 confirmations: Some(confirmations),
    //                 ..
    //             }) if self
    //                 .temporal_safety
    //                 .final_tx(*confirmations, Blockchain::Bitcoin)
    //                 && self.syncer_state.tasks.watched_txs.get(id).is_some() =>
    //             {
    //                 self.syncer_state.handle_tx_confs(
    //                     id,
    //                     &Some(*confirmations),
    //                     self.swap_id(),
    //                     self.temporal_safety.btc_finality_thr,
    //                 );
    //                 let txlabel = self.syncer_state.tasks.watched_txs.get(id).unwrap();
    //                 // saving requests of interest for later replaying latest event
    //                 // TODO MAYBE: refactor this block into following TxLabel match as an outer block with inner matching again
    //                 match &txlabel {
    //                     TxLabel::Lock => {
    //                         self.syncer_state.lock_tx_confs = Some(request.clone());
    //                     }
    //                     TxLabel::Cancel => {
    //                         self.syncer_state.cancel_tx_confs = Some(request.clone());
    //                         self.state.sup_cancel_seen();
    //                     }

    //                     _ => {}
    //                 }
    //                 match txlabel {
    //                     TxLabel::Funding => {}
    //                     TxLabel::Lock
    //                         if self.state.a_refundsig()
    //                             && !self.state.a_xmr_locked()
    //                             && !self.state.a_buy_published()
    //                             && self.state.local_params().is_some()
    //                             && self.state.remote_params().is_some()
    //                             && !self.syncer_state.acc_lock_watched() =>
    //                     {
    //                         self.state.a_sup_refundsig_btclocked();
    //                         // TODO: implement state management here?
    //                         if let (
    //                             Some(Params::Alice(alice_params)),
    //                             Some(Params::Bob(bob_params)),
    //                         ) = (&self.state.local_params(), &self.state.remote_params())
    //                         {
    //                             let (spend, view) =
    //                                 aggregate_xmr_spend_view(alice_params, bob_params);
    //                             let viewpair = monero::ViewPair { spend, view };
    //                             let address = monero::Address::from_viewpair(
    //                                 self.syncer_state.network.into(),
    //                                 &viewpair,
    //                             );
    //                             let swap_id = self.swap_id();
    //                             let amount = self.syncer_state.monero_amount;
    //                             info!(
    //                                 "{} | Send {} to {}",
    //                                 swap_id.bright_blue_italic(),
    //                                 amount.bright_green_bold(),
    //                                 address.addr(),
    //                             );
    //                             self.state.a_sup_required_funding_amount(amount);
    //                             let funding_request = Request::FundingInfo(
    //                                 FundingInfo::Monero(MoneroFundingInfo {
    //                                     swap_id,
    //                                     address,
    //                                     amount,
    //                                 }),
    //                             );
    //                             self.syncer_state.awaiting_funding = true;
    //                             if let Some(enquirer) = self.enquirer.clone() {
    //                                 endpoints.send_to(
    //                                     ServiceBus::Ctl,
    //                                     self.identity(),
    //                                     enquirer,
    //                                     funding_request,
    //                                 )?
    //                             }
    //                             let txlabel = TxLabel::AccLock;
    //                             if !self.syncer_state.is_watched_addr(&txlabel) {
    //                                 let watch_addr_task = self
    //                                     .syncer_state
    //                                     .watch_addr_xmr(spend, view, txlabel, None);
    //                                 endpoints.send_to(
    //                                     ServiceBus::Ctl,
    //                                     self.identity(),
    //                                     self.syncer_state.monero_syncer(),
    //                                     Request::SyncerTask(watch_addr_task),
    //                                 )?;
    //                             }
    //                         } else {
    //                             error!(
    //                                 "local_params or remote_params not set for Alice, state {}",
    //                                 self.state
    //                             )
    //                         }
    //                     }
    //                     TxLabel::Lock
    //                         if self.temporal_safety.valid_cancel(*confirmations)
    //                             && self.state.safe_cancel()
    //                             && self.txs.contains_key(&TxLabel::Cancel) =>
    //                     {
    //                         let (tx_label, cancel_tx) =
    //                             self.txs.remove_entry(&TxLabel::Cancel).unwrap();
    //                         self.broadcast(cancel_tx, tx_label, endpoints)?
    //                     }
    //                     TxLabel::Lock
    //                         if self.temporal_safety.safe_buy(*confirmations)
    //                             && self.state.swap_role() == SwapRole::Alice
    //                             && self.state.a_refundsig()
    //                             && !self.state.a_buy_published()
    //                             && !self.state.cancel_seen()
    //                             && !self.state.a_overfunded() // don't publish buy in case we overfunded
    //                             && self.txs.contains_key(&TxLabel::Buy)
    //                             && self.state.remote_params().is_some()
    //                             && self.state.local_params().is_some() =>
    //                     {
    //                         let xmr_locked = self.state.a_xmr_locked();
    //                         let btc_locked = self.state.a_btc_locked();
    //                         let overfunded = self.state.a_overfunded();
    //                         let required_funding_amount =
    //                             self.state.a_required_funding_amount();
    //                         if let Some((txlabel, buy_tx)) =
    //                             self.txs.remove_entry(&TxLabel::Buy)
    //                         {
    //                             self.broadcast(buy_tx, txlabel, endpoints)?;
    //                             self.state = State::Alice(AliceState::RefundSigA {
    //                                 local_params: self.state.local_params().cloned().unwrap(),
    //                                 buy_published: true, // FIXME
    //                                 btc_locked,
    //                                 xmr_locked,
    //                                 cancel_seen: false,
    //                                 refund_seen: false,
    //                                 remote_params: self.state.remote_params().unwrap(),
    //                                 last_checkpoint_type: self
    //                                     .state
    //                                     .last_checkpoint_type()
    //                                     .unwrap(),
    //                                 required_funding_amount,
    //                                 overfunded,
    //                             });
    //                         } else {
    //                             warn!(
    //                                 "Alice doesn't have the buy tx, probably didnt receive \
    //                                      the buy signature yet: {}",
    //                                 self.state
    //                             );
    //                         }
    //                     }
    //                     TxLabel::Lock
    //                         if self
    //                             .temporal_safety
    //                             .stop_funding_before_cancel(*confirmations)
    //                             && self.state.safe_cancel()
    //                             && self.state.swap_role() == SwapRole::Alice
    //                             && self.syncer_state.awaiting_funding =>
    //                     {
    //                         warn!(
    //                             "{} | Alice, the swap may be cancelled soon. Do not fund anymore",
    //                             self.swap_id.bright_blue_italic()
    //                         );
    //                         self.syncer_state.awaiting_funding = false;
    //                         endpoints.send_to(
    //                             ServiceBus::Ctl,
    //                             self.identity(),
    //                             ServiceId::Farcasterd,
    //                             Request::FundingCanceled(Blockchain::Monero),
    //                         )?
    //                     }

    //                     TxLabel::Cancel
    //                         if self.temporal_safety.valid_punish(*confirmations)
    //                             && self.state.a_refundsig()
    //                             && self.state.a_xmr_locked()
    //                             && self.txs.contains_key(&TxLabel::Punish)
    //                             && !self.state.a_refund_seen() =>
    //                     {
    //                         trace!("Alice publishes punish tx");

    //                         let (tx_label, punish_tx) =
    //                             self.txs.remove_entry(&TxLabel::Punish).unwrap();
    //                         // syncer's watch punish tx task
    //                         if !self.syncer_state.is_watched_tx(&tx_label) {
    //                             let txid = punish_tx.txid();
    //                             let task = self.syncer_state.watch_tx_btc(txid, tx_label);
    //                             endpoints.send_to(
    //                                 ServiceBus::Ctl,
    //                                 self.identity(),
    //                                 self.syncer_state.bitcoin_syncer(),
    //                                 Request::SyncerTask(task),
    //                             )?;
    //                         }

    //                         self.broadcast(punish_tx, tx_label, endpoints)?;
    //                     }

    //                     TxLabel::Cancel
    //                         if self.temporal_safety.safe_refund(*confirmations)
    //                             && (self.state.b_buy_sig() || self.state.b_core_arb())
    //                             && self.txs.contains_key(&TxLabel::Refund) =>
    //                     {
    //                         trace!("here Bob publishes refund tx");
    //                         let (tx_label, refund_tx) =
    //                             self.txs.remove_entry(&TxLabel::Refund).unwrap();
    //                         self.broadcast(refund_tx, tx_label, endpoints)?;
    //                     }
    //                     TxLabel::Cancel
    //                         if (self.state.swap_role() == SwapRole::Alice
    //                             && !self.state.a_xmr_locked()) =>
    //                     {
    //                         warn!(
    //                             "{} | Alice, this swap was canceled. Do not fund anymore.",
    //                             self.swap_id.bright_blue_italic()
    //                         );
    //                         if self.syncer_state.awaiting_funding {
    //                             endpoints.send_to(
    //                                 ServiceBus::Ctl,
    //                                 self.identity(),
    //                                 ServiceId::Farcasterd,
    //                                 Request::FundingCanceled(Blockchain::Monero),
    //                             )?;
    //                             self.syncer_state.awaiting_funding = false;
    //                         }
    //                         self.state_update(
    //                             endpoints,
    //                             State::Alice(AliceState::FinishA(Outcome::Refund)),
    //                         )?;
    //                         let abort_all = Task::Abort(Abort {
    //                             task_target: TaskTarget::AllTasks,
    //                             respond: Boolean::False,
    //                         });
    //                         endpoints.send_to(
    //                             ServiceBus::Ctl,
    //                             self.identity(),
    //                             self.syncer_state.monero_syncer(),
    //                             Request::SyncerTask(abort_all.clone()),
    //                         )?;
    //                         endpoints.send_to(
    //                             ServiceBus::Ctl,
    //                             self.identity(),
    //                             self.syncer_state.bitcoin_syncer(),
    //                             Request::SyncerTask(abort_all),
    //                         )?;
    //                         let swap_success_req = Request::SwapOutcome(Outcome::Refund);
    //                         self.send_wallet(
    //                             ServiceBus::Ctl,
    //                             endpoints,
    //                             swap_success_req.clone(),
    //                         )?;
    //                         self.send_ctl(endpoints, ServiceId::Farcasterd, swap_success_req)?;
    //                         self.txs.remove(&TxLabel::Buy);
    //                         self.txs.remove(&TxLabel::Cancel);
    //                         self.txs.remove(&TxLabel::Punish);
    //                     }
    //                     TxLabel::Buy
    //                         if self
    //                             .temporal_safety
    //                             .final_tx(*confirmations, Blockchain::Bitcoin)
    //                             && self.state.a_refundsig() =>
    //                     {
    //                         // FIXME: swap ends here for alice
    //                         // wallet + farcaster
    //                         self.state_update(
    //                             endpoints,
    //                             State::Alice(AliceState::FinishA(Outcome::Buy)),
    //                         )?;
    //                         let abort_all = Task::Abort(Abort {
    //                             task_target: TaskTarget::AllTasks,
    //                             respond: Boolean::False,
    //                         });
    //                         endpoints.send_to(
    //                             ServiceBus::Ctl,
    //                             self.identity(),
    //                             self.syncer_state.monero_syncer(),
    //                             Request::SyncerTask(abort_all.clone()),
    //                         )?;
    //                         endpoints.send_to(
    //                             ServiceBus::Ctl,
    //                             self.identity(),
    //                             self.syncer_state.bitcoin_syncer(),
    //                             Request::SyncerTask(abort_all),
    //                         )?;
    //                         let swap_success_req = Request::SwapOutcome(Outcome::Buy);
    //                         self.send_wallet(
    //                             ServiceBus::Ctl,
    //                             endpoints,
    //                             swap_success_req.clone(),
    //                         )?;
    //                         self.send_ctl(endpoints, ServiceId::Farcasterd, swap_success_req)?;
    //                         self.txs.remove(&TxLabel::Cancel);
    //                         self.txs.remove(&TxLabel::Punish);
    //                     }
    //                     TxLabel::Buy
    //                         if self.state.swap_role() == SwapRole::Bob
    //                             && self.syncer_state.tasks.txids.contains_key(txlabel) =>
    //                     {
    //                         debug!("request Buy tx task");
    //                         let (txlabel, txid) =
    //                             self.syncer_state.tasks.txids.remove_entry(txlabel).unwrap();
    //                         let task = self.syncer_state.retrieve_tx_btc(txid, txlabel);
    //                         endpoints.send_to(
    //                             ServiceBus::Ctl,
    //                             self.identity(),
    //                             self.syncer_state.bitcoin_syncer(),
    //                             Request::SyncerTask(task),
    //                         )?;
    //                     }
    //                     TxLabel::Refund
    //                         if self.state.swap_role() == SwapRole::Alice
    //                             && !self.state.a_refund_seen()
    //                             && self.syncer_state.tasks.txids.contains_key(txlabel) =>
    //                     {
    //                         debug!("subscribe Refund address task");
    //                         self.state.a_sup_refundsig_refund_seen();
    //                         let (txlabel, txid) =
    //                             self.syncer_state.tasks.txids.remove_entry(txlabel).unwrap();
    //                         let task = self.syncer_state.retrieve_tx_btc(txid, txlabel);
    //                         endpoints.send_to(
    //                             ServiceBus::Ctl,
    //                             self.identity(),
    //                             self.syncer_state.bitcoin_syncer(),
    //                             Request::SyncerTask(task),
    //                         )?;
    //                     }

    //                     TxLabel::Refund if self.state.swap_role() == SwapRole::Bob => {
    //                         let abort_all = Task::Abort(Abort {
    //                             task_target: TaskTarget::AllTasks,
    //                             respond: Boolean::False,
    //                         });
    //                         endpoints.send_to(
    //                             ServiceBus::Ctl,
    //                             self.identity(),
    //                             self.syncer_state.monero_syncer(),
    //                             Request::SyncerTask(abort_all.clone()),
    //                         )?;
    //                         endpoints.send_to(
    //                             ServiceBus::Ctl,
    //                             self.identity(),
    //                             self.syncer_state.bitcoin_syncer(),
    //                             Request::SyncerTask(abort_all),
    //                         )?;
    //                         self.state_update(
    //                             endpoints,
    //                             State::Bob(BobState::FinishB(Outcome::Refund)),
    //                         )?;
    //                         let swap_success_req = Request::SwapOutcome(Outcome::Refund);
    //                         self.send_ctl(
    //                             endpoints,
    //                             ServiceId::Wallet,
    //                             swap_success_req.clone(),
    //                         )?;
    //                         self.send_ctl(endpoints, ServiceId::Farcasterd, swap_success_req)?;
    //                         // remove txs to invalidate outdated states
    //                         self.txs.remove(&TxLabel::Cancel);
    //                         self.txs.remove(&TxLabel::Refund);
    //                         self.txs.remove(&TxLabel::Buy);
    //                         self.txs.remove(&TxLabel::Punish);
    //                     }
    //                     TxLabel::Punish => {
    //                         let abort_all = Task::Abort(Abort {
    //                             task_target: TaskTarget::AllTasks,
    //                             respond: Boolean::False,
    //                         });
    //                         endpoints.send_to(
    //                             ServiceBus::Ctl,
    //                             self.identity(),
    //                             self.syncer_state.monero_syncer(),
    //                             Request::SyncerTask(abort_all.clone()),
    //                         )?;
    //                         endpoints.send_to(
    //                             ServiceBus::Ctl,
    //                             self.identity(),
    //                             self.syncer_state.bitcoin_syncer(),
    //                             Request::SyncerTask(abort_all),
    //                         )?;
    //                         match self.state.swap_role() {
    //                             SwapRole::Alice => self.state_update(
    //                                 endpoints,
    //                                 State::Alice(AliceState::FinishA(Outcome::Punish)),
    //                             )?,
    //                             SwapRole::Bob => {
    //                                 warn!("{}", "You were punished!".err());
    //                                 self.state_update(
    //                                     endpoints,
    //                                     State::Bob(BobState::FinishB(Outcome::Punish)),
    //                                 )?
    //                             }
    //                         }
    //                         let swap_success_req = Request::SwapOutcome(Outcome::Punish);
    //                         self.send_ctl(
    //                             endpoints,
    //                             ServiceId::Wallet,
    //                             swap_success_req.clone(),
    //                         )?;
    //                         self.send_ctl(endpoints, ServiceId::Farcasterd, swap_success_req)?;
    //                         // remove txs to invalidate outdated states
    //                         self.txs.remove(&TxLabel::Cancel);
    //                         self.txs.remove(&TxLabel::Refund);
    //                         self.txs.remove(&TxLabel::Buy);
    //                         self.txs.remove(&TxLabel::Punish);
    //                     }
    //                     tx_label => trace!(
    //                         "{} | Tx {} with {} confirmations evokes no response in state {}",
    //                         self.swap_id.bright_blue_italic(),
    //                         tx_label.bright_white_bold(),
    //                         confirmations,
    //                         &self.state
    //                     ),
    //                 }
    //             }
    //             Event::TransactionConfirmations(TransactionConfirmations {
    //                 id,
    //                 confirmations,
    //                 ..
    //             }) => {
    //                 self.syncer_state.handle_tx_confs(
    //                     id,
    //                     confirmations,
    //                     self.swap_id(),
    //                     self.temporal_safety.btc_finality_thr,
    //                 );
    //             }
    //             Event::TransactionBroadcasted(event) => {
    //                 self.syncer_state.transaction_broadcasted(event)
    //             }
    //             Event::TaskAborted(event) => {
    //                 debug!("{}", event)
    //             }
    //             Event::SweepSuccess(SweepSuccess { id, .. })
    //                 if self.state.b_outcome_abort()
    //                     && self.syncer_state.tasks.sweeping_addr.is_some()
    //                     && &self.syncer_state.tasks.sweeping_addr.unwrap() == id =>
    //             {
    //                 self.state_update(
    //                     endpoints,
    //                     State::Bob(BobState::FinishB(Outcome::Abort)),
    //                 )?;
    //                 endpoints.send_to(
    //                     ServiceBus::Ctl,
    //                     self.identity(),
    //                     ServiceId::Farcasterd,
    //                     Request::FundingCanceled(Blockchain::Bitcoin),
    //                 )?;
    //                 self.abort_swap(endpoints)?;
    //             }
    //             Event::SweepSuccess(event) => {
    //                 debug!("{}", event)
    //             }
    //             Event::TransactionRetrieved(event) => {
    //                 debug!("{}", event)
    //             }
    //             Event::FeeEstimation(FeeEstimation {
    //                 fee_estimations:
    //                     FeeEstimations::BitcoinFeeEstimation {
    //                         high_priority_sats_per_kvbyte,
    //                         low_priority_sats_per_kvbyte: _,
    //                     },
    //                 id: _,
    //             }) => {
    //                 // FIXME handle low priority as well
    //                 info!("fee: {} sat/kvB", high_priority_sats_per_kvbyte);
    //                 self.syncer_state.btc_fee_estimate_sat_per_kvb =
    //                     Some(*high_priority_sats_per_kvbyte);

    //                 if self.state.remote_commit().is_some()
    //                     && (self.state.commit() || self.state.reveal())
    //                     && self.state.b_address().is_some()
    //                     && self.syncer_state.btc_fee_estimate_sat_per_kvb.is_some()
    //                 {
    //                     let success = self.continue_deferred_requests(endpoints, source, |i| {
    //                         matches!(
    //                             &i,
    //                             &PendingRequest {
    //                                 bus_id: ServiceBus::Msg,
    //                                 request: Request::Protocol(Msg::Reveal(
    //                                     Reveal::AliceParameters(..)
    //                                 )),
    //                                 dest: ServiceId::Swap(..),
    //                                 ..
    //                             }
    //                         )
    //                     });
    //                     if success {
    //                         debug!("successfully dispatched reveal:aliceparams")
    //                     } else {
    //                         debug!("failed dispatching reveal:aliceparams, maybe sent before")
    //                     }
    //                 }
    //             }
    //         }
    //     }

    //     _ => {
    //         error!("Request is not supported by the CTL interface {}", request);
    //         return Err(Error::NotSupported(ServiceBus::Ctl, request.get_type()));
    //     }
    // }
}

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
                    let pending_req = PendingRequest::from_event_forward(
                        &event,
                        ServiceBus::Msg,
                        ServiceId::Wallet,
                    );
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
                let pending_request =
                    PendingRequest::from_event_forward(&event, ServiceBus::Msg, ServiceId::Wallet);
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
