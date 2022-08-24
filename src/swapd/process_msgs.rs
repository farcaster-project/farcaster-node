use std::time::{Duration, SystemTime};

use crate::{
    databased::checkpoint_send,
    event::Event,
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
use microservices::esb::Handler;

use super::{
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

            // FIXME: currently only logging
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
                // return Err(err);
                false
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
            Awaiting::TakerP2pMakerCommit => self.taker_p2p_maker_commit(event),
            Awaiting::MakerTakerP2pRevealProof => self.maker_taker_p2p_reveal_proof(event),
            Awaiting::MakerTakerP2pReveal => self.maker_taker_p2p_reveal(event),
            Awaiting::AliceP2pCoreArb => self.alice_p2p_core_arb(event),
            Awaiting::AliceP2pBuySig => self.alice_p2p_buy_sig(event),
            Awaiting::BobP2pRevealAlice => self.bob_p2p_reveal_alice(event),
            Awaiting::BobP2pRefundSigs => self.bob_p2p_refund_sigs(event),

            // Ctl bus
            Awaiting::TakerCtlTakeSwap => self.taker_ctl_take_swap(event),
            Awaiting::MakerCtlRevealProof => self.maker_ctl_reveal_proof(event),
            Awaiting::MakerCtlMakeSwap => self.maker_ctl_make_swap(event),
            Awaiting::MakerTakerCtlFundingUpdated => self.maker_taker_ctl_funding_updated(event),
            Awaiting::BobCtlCoreArb => self.bob_ctl_core_arb(event),
            Awaiting::AliceCtlRefundSigs => self.alice_ctl_refund_sigs(event),
            Awaiting::BobCtlBuySig => self.bob_ctl_buy_sig(event),
            Awaiting::BobCtlTxLock => self.bob_ctl_tx_lock(event),
            Awaiting::CtlTx => self.ctl_tx(event),
            Awaiting::CtlSweepMonero => self.ctl_sweep_monero(event),
            Awaiting::CtlTerminate => self.ctl_terminate(),
            Awaiting::CtlSweepBitcoin => self.ctl_sweep_bitcoin(event),
            Awaiting::CtlPeerReconnected => self.ctl_peer_reconnected(event),
            Awaiting::CtlCheckpoint => self.ctl_checkpoint(event),

            // Ctl + Rpc bus
            Awaiting::CtlRpcAbortSimple => self.ctl_rpc_abort_simple(event),
            Awaiting::BobCtlRpcAbortBob => self.bob_ctl_rpc_abort_bob(event),
            Awaiting::CtlRpcAbortBlocked => self.ctl_rpc_abort_blocked(event),

            // Rpc bus
            Awaiting::RpcGetInfo => self.rpc_get_info(event),

            // Syncer bus

            // Others
            Awaiting::Unknown => Err(Error::Farcaster(s!("Invalid state for this event"))),
        }
    }
}

/// Ctl bus processing fns
impl Runtime {
    fn taker_ctl_take_swap(&mut self, event: Event<Request>) -> Result<(), Error> {
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

    fn maker_ctl_reveal_proof(&mut self, event: Event<Request>) -> Result<(), Error> {
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
    fn maker_ctl_make_swap(&mut self, event: Event<Request>) -> Result<(), Error> {
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

    fn maker_taker_ctl_funding_updated(&mut self, event: Event<Request>) -> Result<(), Error> {
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

    fn bob_ctl_core_arb(&mut self, event: Event<Request>) -> Result<(), Error> {
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

    fn alice_ctl_refund_sigs(&mut self, event: Event<Request>) -> Result<(), Error> {
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

    fn bob_ctl_buy_sig(&mut self, event: Event<Request>) -> Result<(), Error> {
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

    fn bob_ctl_tx_lock(&mut self, event: Event<Request>) -> Result<(), Error> {
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

    fn ctl_tx(&mut self, event: Event<Request>) -> Result<(), Error> {
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
                        self.handle_ctl(event.endpoints, source, lock_tx_confs_req)?;
                    }
                }
                Tx::Refund(_) | Tx::Punish(_) => {
                    if let Some(cancel_tx_confs_req) = self.syncer_state.cancel_tx_confs.clone() {
                        self.handle_ctl(event.endpoints, source, cancel_tx_confs_req)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn ctl_sweep_monero(&mut self, event: Event<Request>) -> Result<(), Error> {
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
    fn ctl_terminate(&mut self) -> Result<(), Error> {
        info!(
            "{} | {}",
            self.swap_id().bright_blue_italic(),
            format!("Terminating {}", self.identity()).bright_white_bold()
        );
        std::process::exit(0);
    }
    fn ctl_sweep_bitcoin(&mut self, event: Event<Request>) -> Result<(), Error> {
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
    fn ctl_rpc_abort_simple(&mut self, event: Event<Request>) -> Result<(), Error> {
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

    fn bob_ctl_rpc_abort_bob(&mut self, event: Event<Request>) -> Result<(), Error> {
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
    fn ctl_rpc_abort_blocked(&mut self, event: Event<Request>) -> Result<(), Error> {
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

    fn rpc_get_info(&mut self, event: Event<Request>) -> Result<(), Error> {
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
    fn ctl_peer_reconnected(&mut self, event: Event<Request>) -> Result<(), Error> {
        for msg in self.pending_peer_request.clone().iter() {
            self.send_peer(event.endpoints, msg.clone())?;
        }
        self.pending_peer_request.clear();
        Ok(())
    }

    fn ctl_checkpoint(&mut self, event: Event<Request>) -> Result<(), Error> {
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
}

/// Msg bus processing fns
impl Runtime {
    fn maker_taker_p2p_reveal(&mut self, mut event: Event<Request>) -> Result<(), Error> {
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

    fn maker_taker_p2p_reveal_proof(&mut self, mut event: Event<Request>) -> Result<(), Error> {
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

    fn bob_p2p_reveal_alice(&mut self, event: Event<Request>) -> Result<(), Error> {
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

    fn taker_p2p_maker_commit(&mut self, mut event: Event<Request>) -> Result<(), Error> {
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

    fn alice_p2p_core_arb(&mut self, mut event: Event<Request>) -> Result<(), Error> {
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

    fn bob_p2p_refund_sigs(&mut self, mut event: Event<Request>) -> Result<(), Error> {
        self.state.sup_received_refund_procedure_signatures();
        event.forward(ServiceBus::Msg, ServiceId::Wallet)?;
        Ok(())
    }

    fn alice_p2p_buy_sig(&mut self, mut event: Event<Request>) -> Result<(), Error> {
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
