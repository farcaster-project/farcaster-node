// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use super::{
    swap_state::{SwapStateMachine, SwapStateMachineExecutor},
    syncer_client::{SyncerState, SyncerTasks},
    temporal_safety::TemporalSafety,
    StateReport,
};
use crate::service::Endpoints;
use crate::swapd::Opts;
use crate::syncerd::bitcoin_syncer::p2wpkh_signed_tx_fee;
use crate::syncerd::types::{Event, TransactionConfirmations};
use crate::syncerd::{FeeEstimation, FeeEstimations};
use crate::{
    bus::ctl::{BitcoinFundingInfo, Checkpoint, CtlMsg, FundingInfo, Params},
    bus::info::{InfoMsg, SwapInfo},
    bus::p2p::{Commit, PeerMsg, Reveal},
    bus::sync::SyncMsg,
    bus::{BusMsg, Outcome, ServiceBus},
    syncerd::{HeightChanged, TransactionRetrieved, XmrAddressAddendum},
};
use crate::{CtlServer, Error, LogStyle, Service, ServiceConfig, ServiceId};

use std::time::{Duration, SystemTime};
use std::{any::Any, collections::HashMap};

use bitcoin::Txid;
use colored::ColoredString;
use farcaster_core::{
    blockchain::Blockchain,
    crypto::{CommitmentEngine, SharedKeyId},
    monero::SHARED_VIEW_KEY_ID,
    role::{SwapRole, TradeRole},
    swap::btcxmr::{Deal, DealParameters, Parameters},
    swap::SwapId,
    transaction::TxLabel,
};

use internet2::addr::{NodeAddr, NodeId};
use microservices::esb::{self, Handler};
use strict_encoding::{StrictDecode, StrictEncode};

pub fn run(config: ServiceConfig, opts: Opts) -> Result<(), Error> {
    let Opts {
        swap_id,
        deal,
        trade_role: local_trade_role,
        arbitrating_finality,
        arbitrating_safety,
        accordant_finality,
        ..
    } = opts;

    let DealParameters {
        cancel_timelock,
        punish_timelock,
        maker_role, // SwapRole of maker (Alice or Bob)
        network,
        ..
    } = deal.parameters;

    // alice or bob
    let local_swap_role = match local_trade_role {
        TradeRole::Maker => maker_role,
        TradeRole::Taker => maker_role.other(),
    };

    let swap_state_machine = match (local_swap_role, local_trade_role) {
        (SwapRole::Alice, TradeRole::Maker) => SwapStateMachine::StartMaker(SwapRole::Alice),
        (SwapRole::Bob, TradeRole::Maker) => SwapStateMachine::StartMaker(SwapRole::Bob),
        (SwapRole::Alice, TradeRole::Taker) => SwapStateMachine::StartTaker(SwapRole::Alice),
        (SwapRole::Bob, TradeRole::Taker) => SwapStateMachine::StartTaker(SwapRole::Bob),
    };
    info!(
        "{}: {}",
        "Starting swap".to_string().bright_green_bold(),
        swap_id.swap_id()
    );

    let temporal_safety = TemporalSafety {
        cancel_timelock: cancel_timelock.as_u32(),
        punish_timelock: punish_timelock.as_u32(),
        btc_finality_thr: arbitrating_finality.into(),
        race_thr: arbitrating_safety.into(),
        xmr_finality_thr: accordant_finality.into(),
        sweep_monero_thr: crate::swapd::temporal_safety::SWEEP_MONERO_THRESHOLD,
    };

    temporal_safety.valid_params()?;
    let tasks = SyncerTasks {
        counter: 0,
        watched_addrs: none!(),
        watched_txs: none!(),
        retrieving_txs: none!(),
        sweeping_addr: none!(),
        broadcasting_txs: none!(),
        txids: none!(),
        final_txs: none!(),
        tasks: none!(),
    };
    let syncer_state = SyncerState {
        swap_id,
        tasks,
        monero_height: 0,
        bitcoin_height: 0,
        confirmation_bound: 50000,
        lock_tx_confs: None,
        cancel_tx_confs: None,
        buy_tx_confs: None,
        network,
        bitcoin_syncer: ServiceId::Syncer(Blockchain::Bitcoin, network),
        monero_syncer: ServiceId::Syncer(Blockchain::Monero, network),
        awaiting_funding: false,
        xmr_addr_addendum: None,
        btc_fee_estimate_sat_per_kvb: None,
        confirmations: none!(),
    };

    let state_report = StateReport::new("Start".to_string(), &temporal_safety, &syncer_state);

    let runtime = Runtime {
        swap_id,
        identity: ServiceId::Swap(swap_id),
        peer_service: ServiceId::dummy_peer_service_id(NodeAddr {
            id: NodeId::from(deal.node_id.clone()), // node_id is bitcoin::Pubkey
            addr: deal.peer_address,                // peer_address is InetSocketAddr
        }),
        connected: false,
        started: SystemTime::now(),
        syncer_state,
        temporal_safety,
        enquirer: None,
        pending_peer_request: none!(),
        txs: none!(),
        deal,
        local_trade_role,
        local_swap_role,
        latest_state_report: state_report,
        monero_address_creation_height: None,
        swap_state_machine,
        unhandled_peer_message: None, // The last message we received and was not handled by the state machine
    };
    let broker = false;
    Service::run(config, runtime, broker)
}

// FIXME: State enum should carry over the data that is accumulated over time,
// and corresponding lines should be removed from Runtime
pub struct Runtime {
    pub swap_id: SwapId,
    pub identity: ServiceId,
    pub peer_service: ServiceId,
    pub connected: bool,
    pub started: SystemTime,
    pub enquirer: Option<ServiceId>,
    pub syncer_state: SyncerState,
    pub temporal_safety: TemporalSafety,
    pub pending_peer_request: Vec<PeerMsg>, // Peer requests that failed and are waiting for reconnection
    pub txs: HashMap<TxLabel, bitcoin::Transaction>,
    pub deal: Deal,
    pub local_trade_role: TradeRole,
    pub local_swap_role: SwapRole,
    pub latest_state_report: StateReport,
    pub monero_address_creation_height: Option<u64>,
    pub swap_state_machine: SwapStateMachine,
    pub unhandled_peer_message: Option<PeerMsg>,
}

#[derive(Debug, Clone, Display, StrictEncode, StrictDecode)]
#[display("checkpoint-swapd")]
pub struct CheckpointSwapd {
    pub state: SwapStateMachine,
    pub pending_msg: Option<PeerMsg>,
    pub enquirer: Option<ServiceId>,
    pub xmr_addr_addendum: Option<XmrAddressAddendum>,
    pub temporal_safety: TemporalSafety,
    pub txs: Vec<(TxLabel, bitcoin::Transaction)>,
    pub txids: Vec<(TxLabel, Txid)>,
    pub pending_broadcasts: Vec<bitcoin::Transaction>,
    pub local_trade_role: TradeRole,
    pub connected_counterparty_node_id: Option<NodeId>,
    pub deal: Deal,
    pub monero_address_creation_height: Option<u64>,
}

impl CtlServer for Runtime {}

impl esb::Handler<ServiceBus> for Runtime {
    type Request = BusMsg;
    type Error = Error;

    fn identity(&self) -> ServiceId {
        self.identity.clone()
    }

    fn handle(
        &mut self,
        endpoints: &mut Endpoints,
        bus: ServiceBus,
        source: ServiceId,
        request: BusMsg,
    ) -> Result<(), Self::Error> {
        match (bus, request) {
            // Peer-to-peer message bus, only accept peer message
            (ServiceBus::Msg, BusMsg::P2p(req)) => {
                self.handle_msg(endpoints, source, req)?;
                self.report_potential_state_change(endpoints)
            }
            // Control bus for issuing control commands, only accept Ctl message
            (ServiceBus::Ctl, BusMsg::Ctl(req)) => {
                self.handle_ctl(endpoints, source, req)?;
                self.report_potential_state_change(endpoints)
            }
            // Info command bus, only accept Info message
            (ServiceBus::Info, BusMsg::Info(req)) => self.handle_info(endpoints, source, req),
            // Syncer event bus for blockchain tasks and events, only accept Sync message
            (ServiceBus::Sync, BusMsg::Sync(req)) => {
                self.handle_sync(endpoints, source, req)?;
                self.report_potential_state_change(endpoints)
            }
            // All other pairs are not supported
            (bus, req) => Err(Error::NotSupported(bus, req.to_string())),
        }
    }

    fn handle_err(&mut self, _: &mut Endpoints, _: esb::Error<ServiceId>) -> Result<(), Error> {
        // We do nothing and do not propagate error; it's already being reported
        // with `error!` macro by the controller. If we propagate error here
        // this will make whole daemon panic
        Ok(())
    }
}

impl Runtime {
    pub fn send_peer(&mut self, endpoints: &mut Endpoints, msg: PeerMsg) -> Result<(), Error> {
        self.log_trace(format!(
            "sending peer message {} to {}",
            msg, self.peer_service
        ));
        if let Err(error) = endpoints.send_to(
            ServiceBus::Msg,
            self.identity(),
            self.peer_service.clone(),
            BusMsg::P2p(msg.clone()),
        ) {
            self.log_error(format!(
                "could not send message {} to {} due to {}",
                msg, self.peer_service, error
            ));
            self.connected = false;
            self.log_warn(
                "notifying farcasterd of peer error, farcasterd will attempt to reconnect",
            );
            endpoints.send_to(
                ServiceBus::Ctl,
                self.identity(),
                ServiceId::Farcasterd,
                BusMsg::Ctl(CtlMsg::PeerdUnreachable(self.peer_service.clone())),
            )?;
            self.pending_peer_request.push(msg);
        }
        Ok(())
    }

    pub fn swap_id(&self) -> SwapId {
        match self.identity {
            ServiceId::Swap(swap_id) => swap_id,
            _ => {
                unreachable!("not ServiceId::Swap")
            }
        }
    }

    pub fn broadcast(
        &mut self,
        tx: bitcoin::Transaction,
        tx_label: TxLabel,
        endpoints: &mut Endpoints,
    ) -> Result<(), Error> {
        self.log_info(format!(
            "Broadcasting {} tx({})",
            tx_label.label(),
            tx.txid().tx_hash()
        ));
        let task = self.syncer_state.broadcast(tx);
        Ok(endpoints.send_to(
            ServiceBus::Sync,
            self.identity(),
            self.syncer_state.bitcoin_syncer(),
            BusMsg::Sync(SyncMsg::Task(task)),
        )?)
    }

    fn handle_msg(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: PeerMsg,
    ) -> Result<(), Error> {
        // Check if message are from consistent peer source
        if matches!(source, ServiceId::Peer(..)) && self.peer_service != source {
            let msg = format!(
                "Incorrect peer connection: expected {}, found {}",
                self.peer_service, source
            );
            self.log_error(&msg);
            return Err(Error::Farcaster(msg));
        }

        if request.swap_id() != self.swap_id() {
            let msg = format!(
                "{} | Incorrect swap_id: expected {}, found {}",
                self.swap_id.bright_blue_italic(),
                self.swap_id(),
                request.swap_id(),
            );
            self.log_error(&msg);
            return Err(Error::Farcaster(msg));
        }

        match request {
            // bob and alice
            PeerMsg::Abort(_) => {
                return Err(Error::Farcaster("Abort not yet supported".to_string()));
            }

            PeerMsg::Ping(_) | PeerMsg::Pong(_) | PeerMsg::PingPeer => {
                return Err(Error::Farcaster(
                    "Ping/Pong must remain in peerd, not supported in swapd".to_string(),
                ));
            }
            _ => {}
        }

        self.execute_state_machine(endpoints, BusMsg::P2p(request), source)?;

        Ok(())
    }

    pub fn handle_ctl(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: CtlMsg,
    ) -> Result<(), Error> {
        match request {
            CtlMsg::Hello => {
                self.log_info(format!(
                    "Service {} daemon is now connected",
                    source.bright_green_bold(),
                ));
            }
            CtlMsg::Terminate if source == ServiceId::Farcasterd => {
                self.log_info(format!("Terminating {}", self.identity()).label());
                std::process::exit(0);
            }

            CtlMsg::Disconnected => {
                self.connected = false;
            }

            CtlMsg::Reconnected => {
                self.connected = true;
            }

            // Set the reconnected service id. This can happen if this is a
            // maker launched swap after restoration and the taker reconnects,
            // after a manual connect call, or a new connection with the same
            // node address is established
            CtlMsg::PeerdReconnected(service_id) => {
                self.log_info(format!("Peer {} reconnected", service_id));
                self.peer_service = service_id.clone();
                self.connected = true;
                for msg in self.pending_peer_request.clone().iter() {
                    self.send_peer(endpoints, msg.clone())?;
                }
                self.pending_peer_request.clear();
            }

            CtlMsg::FailedPeerMessage(msg) => {
                self.log_warn(format!(
                    "Sending the peer message {} failed. Adding to pending peer requests",
                    msg
                ));
                self.pending_peer_request.push(msg);
            }

            CtlMsg::Checkpoint(Checkpoint { swap_id: _, state }) => {
                let CheckpointSwapd {
                    pending_msg,
                    enquirer,
                    temporal_safety,
                    mut txs,
                    txids,
                    pending_broadcasts,
                    xmr_addr_addendum,
                    local_trade_role,
                    monero_address_creation_height,
                    state,
                    ..
                } = state;
                self.log_info("Restoring swap");
                self.swap_state_machine = state;
                self.enquirer = enquirer;
                self.temporal_safety = temporal_safety;
                self.monero_address_creation_height = monero_address_creation_height;
                // We need to update the peerd for the pending requests in case of reconnect
                self.local_trade_role = local_trade_role;
                self.txs = txs.drain(..).collect();
                self.log_trace("Watch height bitcoin");
                let watch_height_bitcoin = self.syncer_state.watch_height(Blockchain::Bitcoin);
                endpoints.send_to(
                    ServiceBus::Sync,
                    self.identity(),
                    self.syncer_state.bitcoin_syncer(),
                    BusMsg::Sync(SyncMsg::Task(watch_height_bitcoin)),
                )?;

                self.log_trace("Watch height monero");
                let watch_height_monero = self.syncer_state.watch_height(Blockchain::Monero);
                endpoints.send_to(
                    ServiceBus::Sync,
                    self.identity(),
                    self.syncer_state.monero_syncer(),
                    BusMsg::Sync(SyncMsg::Task(watch_height_monero)),
                )?;

                self.log_trace("Watching transactions");
                for (tx_label, txid) in txids.iter() {
                    let task = self
                        .syncer_state
                        .watch_tx_btc(txid.clone(), tx_label.clone());
                    endpoints.send_to(
                        ServiceBus::Sync,
                        self.identity(),
                        self.syncer_state.bitcoin_syncer(),
                        BusMsg::Sync(SyncMsg::Task(task)),
                    )?;
                }

                self.log_trace("Broadcasting txs pending broadcast");
                for tx in pending_broadcasts.iter() {
                    let task = self.syncer_state.broadcast(tx.clone());
                    endpoints.send_to(
                        ServiceBus::Sync,
                        self.identity(),
                        self.syncer_state.bitcoin_syncer(),
                        BusMsg::Sync(SyncMsg::Task(task)),
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
                    endpoints.send_to(
                        ServiceBus::Sync,
                        self.identity(),
                        self.syncer_state.monero_syncer(),
                        BusMsg::Sync(SyncMsg::Task(task)),
                    )?;
                }

                if let Some(msg) = pending_msg {
                    self.send_peer(endpoints, msg)?;
                }
            }

            req => {
                self.execute_state_machine(endpoints, BusMsg::Ctl(req), source)?;
            }
        }

        Ok(())
    }

    fn handle_info(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: InfoMsg,
    ) -> Result<(), Error> {
        match request {
            InfoMsg::GetInfo => {
                let connection = self.peer_service.node_addr();
                let info = SwapInfo {
                    swap_id: self.swap_id,
                    connection,
                    connected: self.connected,
                    state: self.latest_state_report.clone(),
                    uptime: SystemTime::now()
                        .duration_since(self.started)
                        .unwrap_or_else(|_| Duration::from_secs(0)),
                    since: self
                        .started
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap_or_else(|_| Duration::from_secs(0))
                        .as_secs(),
                    deal: self.deal.clone(),
                    local_trade_role: self.local_trade_role,
                    local_swap_role: self.deal.swap_role(&self.local_trade_role),
                    connected_counterparty_node_id: get_node_id(&self.peer_service),
                };
                self.send_client_info(endpoints, source, InfoMsg::SwapInfo(info))?;
            }

            req => {
                self.log_error(format!(
                    "BusMsg {} is not supported by the INFO interface",
                    req.to_string()
                ));
            }
        }

        Ok(())
    }

    pub fn handle_sync(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: SyncMsg,
    ) -> Result<(), Error> {
        match request {
            SyncMsg::Event(ref event) if source == self.syncer_state.monero_syncer => {
                match &event {
                    Event::HeightChanged(HeightChanged { height, .. }) => {
                        self.syncer_state
                            .handle_height_change(*height, Blockchain::Monero);
                    }

                    Event::TransactionConfirmations(TransactionConfirmations {
                        id,
                        confirmations,
                        ..
                    }) => {
                        self.syncer_state.handle_tx_confs(
                            id,
                            confirmations,
                            self.swap_id(),
                            self.temporal_safety.xmr_finality_thr,
                        );
                    }

                    Event::AddressTransaction(_) => {}

                    Event::SweepSuccess(_) => {}

                    Event::TaskAborted(_) => {}

                    Event::Empty(_) => {}

                    event => {
                        self.log_error(format!("event not handled {}", event));
                    }
                };
            }

            SyncMsg::Event(ref event) if source == self.syncer_state.bitcoin_syncer => {
                match &event {
                    Event::HeightChanged(HeightChanged { height, .. }) => {
                        self.syncer_state
                            .handle_height_change(*height, Blockchain::Bitcoin);
                    }

                    // This re-triggers the tx fetch event in case the transaction was not detected yet
                    Event::TransactionRetrieved(TransactionRetrieved { id, tx: None })
                        if self.syncer_state.tasks.retrieving_txs.contains_key(id) =>
                    {
                        let (_tx_label, task) =
                            self.syncer_state.tasks.retrieving_txs.get(id).unwrap();
                        std::thread::sleep(core::time::Duration::from_millis(500));
                        endpoints.send_to(
                            ServiceBus::Sync,
                            self.identity(),
                            self.syncer_state.bitcoin_syncer(),
                            BusMsg::Sync(SyncMsg::Task(task.clone())),
                        )?;
                    }

                    Event::TransactionConfirmations(TransactionConfirmations {
                        id,
                        confirmations: Some(confirmations),
                        ..
                    }) if self
                        .temporal_safety
                        .final_tx(*confirmations, Blockchain::Bitcoin)
                        && self.syncer_state.tasks.watched_txs.get(id).is_some() =>
                    {
                        self.syncer_state.handle_tx_confs(
                            id,
                            &Some(*confirmations),
                            self.swap_id(),
                            self.temporal_safety.btc_finality_thr,
                        );
                        let txlabel = self.syncer_state.tasks.watched_txs.get(id);
                        // saving requests of interest for later replaying latest event
                        match txlabel {
                            Some(&TxLabel::Lock) => {
                                self.syncer_state.lock_tx_confs = Some(request.clone());
                            }
                            Some(&TxLabel::Cancel) => {
                                self.syncer_state.cancel_tx_confs = Some(request.clone());
                            }
                            Some(&TxLabel::Buy) => {
                                self.syncer_state.buy_tx_confs = Some(request.clone())
                            }
                            _ => {}
                        }
                    }

                    Event::TransactionConfirmations(TransactionConfirmations {
                        id,
                        confirmations,
                        ..
                    }) => {
                        self.syncer_state.handle_tx_confs(
                            id,
                            confirmations,
                            self.swap_id(),
                            self.temporal_safety.btc_finality_thr,
                        );
                    }

                    Event::TransactionBroadcasted(event) => {
                        self.syncer_state.transaction_broadcasted(event)
                    }

                    Event::AddressTransaction(_) => {}

                    Event::TaskAborted(event) => {
                        self.log_debug(event);
                    }

                    Event::SweepSuccess(event) => {
                        self.log_debug(event);
                    }

                    Event::TransactionRetrieved(event) => {
                        self.log_debug(event);
                    }

                    Event::AddressBalance(event) => {
                        self.log_debug(event);
                    }

                    Event::FeeEstimation(FeeEstimation {
                        fee_estimations:
                            FeeEstimations::BitcoinFeeEstimation {
                                high_priority_sats_per_kvbyte,
                                low_priority_sats_per_kvbyte: _,
                            },
                        id: _,
                    }) => {
                        // FIXME handle low priority as well
                        self.log_info(format!("Fee: {} sat/kvB", high_priority_sats_per_kvbyte));
                        self.syncer_state.btc_fee_estimate_sat_per_kvb =
                            Some(*high_priority_sats_per_kvbyte);
                    }
                    Event::Empty(_) => self.log_debug("empty event not handled for Bitcoin"),

                    Event::HealthResult(_) => self.log_debug("ignoring health result in swapd"),
                };
            }
            _ => {}
        }
        self.execute_state_machine(endpoints, BusMsg::Sync(request), source)?;

        Ok(())
    }
}

impl Runtime {
    fn execute_state_machine(
        &mut self,
        endpoints: &mut Endpoints,
        msg: BusMsg,
        source: ServiceId,
    ) -> Result<(), Error> {
        if let Some(ssm) = SwapStateMachineExecutor::execute(
            self,
            endpoints,
            source.clone(),
            msg.clone(),
            self.swap_state_machine.clone(),
        )? {
            self.swap_state_machine = ssm;
            // On SwapEnd, report immediately to ensure the progress message goes out before the swap is terminated, then let farcasterd know of the outcome.
            if let SwapStateMachine::SwapEnd(outcome) = &self.swap_state_machine {
                let outcome = outcome.clone(); // so we don't borrow self anymore
                self.report_potential_state_change(endpoints)?;
                self.send_ctl(
                    endpoints,
                    ServiceId::Farcasterd,
                    BusMsg::Ctl(CtlMsg::SwapOutcome(outcome)),
                )?;
            }
            if matches!(self.swap_state_machine, SwapStateMachine::SwapEnd(_)) {
                self.report_potential_state_change(endpoints)?;
                return Ok(());
            }
            // Unset previously set unhandled peer message
            if let BusMsg::P2p(peer_msg) = msg {
                if Some(peer_msg.type_id())
                    == self.unhandled_peer_message.as_ref().map(|p| p.type_id())
                {
                    self.unhandled_peer_message = None;
                }
            }
            // Try to handle previously unhandled peer message
            if let Some(peer_msg) = &self.unhandled_peer_message {
                self.handle_msg(endpoints, source.clone(), peer_msg.clone())?;
            }
            // Replay confirmation events to ensure we immediately advance through states that can be skipped
            if let Some(buy_tx_confs_req) = self.syncer_state.buy_tx_confs.clone() {
                self.handle_sync(endpoints, source.clone(), buy_tx_confs_req)?;
            }
            if let Some(lock_tx_confs_req) = self.syncer_state.lock_tx_confs.clone() {
                self.handle_sync(endpoints, source.clone(), lock_tx_confs_req)?;
            }
            if let Some(cancel_tx_confs_req) = self.syncer_state.cancel_tx_confs.clone() {
                self.handle_sync(endpoints, source.clone(), cancel_tx_confs_req)?;
            }
        } else {
            if let BusMsg::P2p(peer_msg) = msg {
                self.unhandled_peer_message = Some(peer_msg);
            }
        }
        Ok(())
    }

    fn report_potential_state_change(&mut self, endpoints: &mut Endpoints) -> Result<(), Error> {
        // Generate a new state report for the clients
        let new_state_report = StateReport::new(
            self.swap_state_machine.to_string(),
            &self.temporal_safety,
            &self.syncer_state,
        );
        if self.latest_state_report != new_state_report {
            let progress = self
                .latest_state_report
                .generate_progress_update_or_transition(&new_state_report);
            self.latest_state_report = new_state_report;
            if let Some(enquirer) = self.enquirer.clone() {
                endpoints.send_to(
                    ServiceBus::Ctl,
                    self.identity(),
                    enquirer,
                    BusMsg::Ctl(CtlMsg::Progress(progress)),
                )?;
            }
        }
        Ok(())
    }

    pub fn ask_bob_to_fund(
        &mut self,
        sat_per_kvb: u64,
        address: bitcoin::Address,
        endpoints: &mut Endpoints,
    ) -> Result<bitcoin::Amount, Error> {
        let swap_id = self.swap_id();
        let vsize = 94;
        let nr_inputs = 1;
        let total_fees =
            bitcoin::Amount::from_sat(p2wpkh_signed_tx_fee(sat_per_kvb, vsize, nr_inputs));
        let amount = self.deal.parameters.arbitrating_amount + total_fees;
        self.log_info(format!(
            "Send {} to {}, this includes {} for the Lock transaction network fees",
            amount.bright_green_bold(),
            address.addr(),
            total_fees.label(),
        ));
        let req = BusMsg::Ctl(CtlMsg::FundingInfo(FundingInfo::Bitcoin(
            BitcoinFundingInfo {
                swap_id,
                address,
                amount,
            },
        )));
        self.syncer_state.awaiting_funding = true;

        if let Some(enquirer) = self.enquirer.clone() {
            endpoints.send_to(ServiceBus::Ctl, self.identity(), enquirer, req)?;
        }
        Ok(amount)
    }

    pub fn taker_commit(
        &mut self,
        endpoints: &mut Endpoints,
        params: Params,
    ) -> Result<Commit, Error> {
        self.log_info(format!(
            "{} to Maker remote peer",
            "Proposing to take swap".bright_white_bold(),
        ));

        let msg = format!(
            "Proposing to take swap {} to Maker remote peer",
            self.swap_id()
        );
        // Ignoring possible reporting errors here and after: do not want to
        // halt the swap just because the client disconnected
        let _ = self.report_progress_message_to(endpoints, &self.enquirer.clone(), msg);

        let engine = CommitmentEngine;
        let commitment = match params {
            Params::Bob(params) => {
                Commit::BobParameters(params.commit_bob(self.swap_id(), &engine))
            }
            Params::Alice(params) => {
                Commit::AliceParameters(params.commit_alice(self.swap_id(), &engine))
            }
        };

        Ok(commitment)
    }

    pub fn maker_commit(
        &mut self,
        endpoints: &mut Endpoints,
        swap_id: SwapId,
        params: Params,
    ) -> Result<Commit, Error> {
        self.log_info(format!(
            "{} as Maker from Taker through peerd {}",
            "Accepting swap".bright_white_bold(),
            self.peer_service.bright_blue_italic()
        ));

        let msg = format!(
            "Accepting swap {} as Maker from Taker through peerd {}",
            swap_id, self.peer_service
        );
        let enquirer = self.enquirer.clone();
        // Ignoring possible reporting errors here and after: do not want to
        // halt the swap just because the client disconnected
        let _ = self.report_progress_message_to(endpoints, &enquirer, msg);

        let engine = CommitmentEngine;
        let commitment = match params.clone() {
            Params::Bob(params) => {
                Commit::BobParameters(params.commit_bob(self.swap_id(), &engine))
            }
            Params::Alice(params) => {
                Commit::AliceParameters(params.commit_alice(self.swap_id(), &engine))
            }
        };

        Ok(commitment)
    }

    pub fn abort_swap(&mut self, endpoints: &mut Endpoints) -> Result<(), Error> {
        let swap_success_req = BusMsg::Ctl(CtlMsg::SwapOutcome(Outcome::FailureAbort));
        self.send_ctl(endpoints, ServiceId::Farcasterd, swap_success_req)?;
        self.log_info("Aborted swap.");
        Ok(())
    }

    pub fn checkpoint_state(
        &mut self,
        endpoints: &mut Endpoints,
        pending_msg: Option<PeerMsg>,
        next_state: SwapStateMachine,
    ) -> Result<(), Error> {
        endpoints.send_to(
            ServiceBus::Ctl,
            self.identity(),
            ServiceId::Database,
            BusMsg::Ctl(CtlMsg::Checkpoint(Checkpoint {
                swap_id: self.swap_id,
                state: CheckpointSwapd {
                    state: next_state,
                    pending_msg,
                    enquirer: self.enquirer.clone(),
                    temporal_safety: self.temporal_safety.clone(),
                    txs: self.txs.clone().drain().collect(),
                    txids: self.syncer_state.tasks.txids.clone().drain().collect(),
                    pending_broadcasts: self.syncer_state.pending_broadcast_txs(),
                    xmr_addr_addendum: self.syncer_state.xmr_addr_addendum.clone(),
                    local_trade_role: self.local_trade_role,
                    connected_counterparty_node_id: get_node_id(&self.peer_service),
                    deal: self.deal.clone(),
                    monero_address_creation_height: self.monero_address_creation_height.clone(),
                },
            })),
        )?;
        Ok(())
    }

    pub fn log_info(&self, msg: impl std::fmt::Display) {
        info!("{} | {}", self.log_prefix(), msg);
    }

    pub fn log_error(&self, msg: impl std::fmt::Display) {
        error!("{} | {}", self.log_prefix(), msg);
    }

    pub fn log_debug(&self, msg: impl std::fmt::Display) {
        debug!("{} | {}", self.log_prefix(), msg);
    }

    pub fn log_trace(&self, msg: impl std::fmt::Display) {
        trace!("{} | {}", self.log_prefix(), msg);
    }

    pub fn log_warn(&self, msg: impl std::fmt::Display) {
        warn!("{} | {}", self.log_prefix(), msg);
    }

    fn log_prefix(&self) -> ColoredString {
        format!(
            "{} as {} {}",
            self.swap_id, self.local_swap_role, self.local_trade_role
        )
        .bright_blue_italic()
    }
}

pub fn get_swap_id(source: &ServiceId) -> Result<SwapId, Error> {
    if let ServiceId::Swap(swap_id) = source {
        Ok(*swap_id)
    } else {
        Err(Error::Farcaster("Not swapd".to_string()))
    }
}

pub fn get_node_id(service: &ServiceId) -> Option<NodeId> {
    if let ServiceId::Peer(_, addr) = service {
        Some(addr.id)
    } else {
        None
    }
}

pub fn aggregate_xmr_spend_view(
    alice_params: &Parameters,
    bob_params: &Parameters,
) -> (monero::PublicKey, monero::PrivateKey) {
    let alice_view = *alice_params
        .accordant_shared_keys
        .clone()
        .into_iter()
        .find(|vk| vk.tag() == &SharedKeyId::new(SHARED_VIEW_KEY_ID))
        .expect("accordant shared keys should always have a view key")
        .elem();
    let bob_view = *bob_params
        .accordant_shared_keys
        .clone()
        .into_iter()
        .find(|vk| vk.tag() == &SharedKeyId::new(SHARED_VIEW_KEY_ID))
        .expect("accordant shared keys should always have a view key")
        .elem();
    (alice_params.spend + bob_params.spend, alice_view + bob_view)
}

/// Parameter processing irrespective of maker & taker role. Return [`Params`] if the commit/reveal
/// matches.
pub fn validate_reveal(reveal: &Reveal, remote_commit: Commit) -> Result<Params, Error> {
    let core_wallet = CommitmentEngine;
    match reveal {
        Reveal::Alice { parameters, .. } => match &remote_commit {
            Commit::AliceParameters(commit) => {
                commit.verify_with_reveal(&core_wallet, parameters.clone())?;
                Ok(Params::Alice(parameters.clone().into_parameters()))
            }
            _ => {
                let err_msg = "expected Some(Commit::Alice(commit))";
                error!("{}", err_msg);
                Err(Error::Farcaster(err_msg.to_string()))
            }
        },
        Reveal::Bob { parameters, .. } => match &remote_commit {
            Commit::BobParameters(commit) => {
                commit.verify_with_reveal(&core_wallet, parameters.clone())?;
                Ok(Params::Bob(parameters.clone().into_parameters()))
            }
            _ => {
                let err_msg = "expected Some(Commit::Bob(commit))";
                error!("{}", err_msg);
                Err(Error::Farcaster(err_msg.to_string()))
            }
        },
    }
}
