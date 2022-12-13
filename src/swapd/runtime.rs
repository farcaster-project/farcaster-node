// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use super::{
    swap_state::{AliceState, BobState, State, SwapCheckpointType},
    syncer_client::{log_tx_created, log_tx_seen, SyncerState, SyncerTasks},
    temporal_safety::TemporalSafety,
    wallet::Wallet,
};
use crate::bus::ctl::{InitMakerSwap, InitTakerSwap};
use crate::service::Endpoints;
use crate::swapd::wallet::{
    HandleBuyProcedureSignatureRes, HandleCoreArbitratingSetupRes,
    HandleRefundProcedureSignaturesRes,
};
use crate::swapd::{Opts, StateReport};
use crate::syncerd::bitcoin_syncer::p2wpkh_signed_tx_fee;
use crate::syncerd::types::{AddressTransaction, Boolean, Event, Task, TransactionConfirmations};
use crate::syncerd::{FeeEstimation, FeeEstimations};
use crate::{
    bus::ctl::{
        BitcoinFundingInfo, Checkpoint, CtlMsg, FundingInfo, MoneroFundingInfo, Params, Tx,
    },
    bus::info::{InfoMsg, SwapInfo},
    bus::p2p::{Commit, PeerMsg, Reveal, TakerCommit},
    bus::sync::SyncMsg,
    bus::{BusMsg, Failure, FailureCode, Outcome, ServiceBus},
    syncerd::{
        Abort, HeightChanged, SweepSuccess, TaskTarget, TransactionRetrieved, XmrAddressAddendum,
    },
};
use crate::{CtlServer, Error, LogStyle, Service, ServiceConfig, ServiceId};

use std::collections::HashMap;
use std::{
    io::Cursor,
    time::{Duration, SystemTime},
};

use bitcoin::util::psbt::serialize::Deserialize;
use bitcoin::Txid;
use farcaster_core::{
    blockchain::Blockchain,
    crypto::{CommitmentEngine, SharedKeyId},
    monero::SHARED_VIEW_KEY_ID,
    role::{SwapRole, TradeRole},
    swap::btcxmr::{Offer, Parameters, PublicOffer},
    swap::SwapId,
    transaction::TxLabel,
};

use internet2::{
    addr::{NodeAddr, NodeId},
    CreateUnmarshaller, TypedEnum, Unmarshall, Unmarshaller,
};
use microservices::esb::{self, Handler};
use strict_encoding::{StrictDecode, StrictEncode};

pub fn run(config: ServiceConfig, opts: Opts) -> Result<(), Error> {
    let Opts {
        swap_id,
        public_offer,
        trade_role: local_trade_role,
        arbitrating_finality,
        arbitrating_safety,
        accordant_finality,
        ..
    } = opts;

    let Offer {
        cancel_timelock,
        punish_timelock,
        maker_role, // SwapRole of maker (Alice or Bob)
        network,
        accordant_amount: monero_amount,
        arbitrating_amount: bitcoin_amount,
        ..
    } = public_offer.offer;

    // alice or bob
    let local_swap_role = match local_trade_role {
        TradeRole::Maker => maker_role,
        TradeRole::Taker => maker_role.other(),
    };

    let init_state = match local_swap_role {
        SwapRole::Alice => State::Alice(AliceState::StartA { local_trade_role }),
        SwapRole::Bob => State::Bob(BobState::StartB { local_trade_role }),
    };
    info!(
        "{}: {}",
        "Starting swap".to_string().bright_green_bold(),
        format!("{:#x}", swap_id).swap_id()
    );
    info!(
        "{} | Initial state: {}",
        swap_id.swap_id(),
        init_state.label()
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
        network,
        bitcoin_syncer: ServiceId::Syncer(Blockchain::Bitcoin, network),
        monero_syncer: ServiceId::Syncer(Blockchain::Monero, network),
        monero_amount,
        bitcoin_amount,
        awaiting_funding: false,
        xmr_addr_addendum: None,
        btc_fee_estimate_sat_per_kvb: None,
        confirmations: none!(),
    };

    let state_report = StateReport::new(&init_state, &temporal_safety, &syncer_state);

    let runtime = Runtime {
        swap_id,
        identity: ServiceId::Swap(swap_id),
        peer_service: ServiceId::dummy_peer_service_id(NodeAddr {
            id: NodeId::from(public_offer.node_id.clone()), // node_id is bitcoin::Pubkey
            addr: public_offer.peer_address,                // peer_address is InetSocketAddr
        }),
        connected: false,
        state: init_state,
        started: SystemTime::now(),
        syncer_state,
        temporal_safety,
        enquirer: None,
        pending_requests: none!(),
        pending_peer_request: none!(),
        txs: none!(),
        public_offer,
        local_trade_role,
        latest_state_report: state_report,
        monero_address_creation_height: None,
        wallet: None,
    };
    let broker = false;
    Service::run(config, runtime, broker)
}

// FIXME: State enum should carry over the data that is accumulated over time,
// and corresponding lines should be removed from Runtime
pub struct Runtime {
    swap_id: SwapId,
    identity: ServiceId,
    peer_service: ServiceId,
    connected: bool,
    state: State,
    started: SystemTime,
    enquirer: Option<ServiceId>,
    syncer_state: SyncerState,
    temporal_safety: TemporalSafety,
    pending_requests: PendingRequests,
    pending_peer_request: Vec<PeerMsg>, // Peer requests that failed and are waiting for reconnection
    txs: HashMap<TxLabel, bitcoin::Transaction>,
    public_offer: PublicOffer,
    local_trade_role: TradeRole,
    latest_state_report: StateReport,
    monero_address_creation_height: Option<u64>,
    wallet: Option<Wallet>,
}

// FIXME Something more meaningful than ServiceId to index
type PendingRequests = HashMap<ServiceId, Vec<PendingRequest>>;

impl PendingRequestsT for PendingRequests {
    fn defer_request(&mut self, key: ServiceId, pending_req: PendingRequest) {
        let pending_reqs = self.entry(key).or_insert(vec![]);
        pending_reqs.push(pending_req);
    }

    fn update_deferred_requests_peer_destination(&mut self, destination: ServiceId) {
        for (_, pending_requests) in self.iter_mut() {
            for pending_request in pending_requests.iter_mut() {
                if pending_request.dest.node_addr() == destination.node_addr() {
                    pending_request.dest = destination.clone();
                }
            }
        }
    }

    fn continue_deferred_requests(
        runtime: &mut Runtime,
        endpoints: &mut Endpoints,
        key: ServiceId,
        predicate: fn(&PendingRequest) -> bool,
    ) -> bool {
        let success = if let Some(pending_reqs) = runtime.pending_requests.remove(&key) {
            let len0 = pending_reqs.len();
            let remaining_pending_reqs: Vec<_> = pending_reqs
                .into_iter()
                .filter_map(|r| {
                    if predicate(&r) {
                        if let Ok(_) = match (&r.bus_id, &r.request) {
                            (ServiceBus::Ctl, BusMsg::Ctl(ctl)) if &r.dest == &runtime.identity => {
                                runtime.handle_ctl(endpoints, r.source.clone(), ctl.clone())
                            }
                            (ServiceBus::Msg, BusMsg::P2p(peer))
                                if &r.dest == &runtime.identity =>
                            {
                                runtime.handle_msg(endpoints, r.source.clone(), peer.clone())
                            }
                            (ServiceBus::Sync, BusMsg::Sync(sync))
                                if &r.dest == &runtime.identity =>
                            {
                                runtime.handle_sync(endpoints, r.source.clone(), sync.clone())
                            }
                            (_, _) => endpoints
                                .send_to(
                                    r.bus_id.clone(),
                                    r.source.clone(),
                                    r.dest.clone(),
                                    r.request.clone(),
                                )
                                .map_err(Into::into),
                        } {
                            None
                        } else {
                            Some(r)
                        }
                    } else {
                        Some(r)
                    }
                })
                .collect();
            let len1 = remaining_pending_reqs.len();
            runtime.pending_requests.insert(key, remaining_pending_reqs);
            if len0 - len1 > 1 {
                error!("consumed more than one request with this predicate")
            }
            len0 > len1
        } else {
            error!("no request consumed with this predicate");
            false
        };
        success
    }
}

trait PendingRequestsT {
    fn defer_request(&mut self, key: ServiceId, pending_req: PendingRequest);

    fn update_deferred_requests_peer_destination(&mut self, destination: ServiceId);

    fn continue_deferred_requests(
        runtime: &mut Runtime, // needed for recursion
        endpoints: &mut Endpoints,
        key: ServiceId,
        predicate: fn(&PendingRequest) -> bool,
    ) -> bool;
}

#[derive(Debug, Clone)]
pub struct PendingRequest {
    source: ServiceId,
    dest: ServiceId,
    bus_id: ServiceBus,
    request: BusMsg,
}

impl PendingRequest {
    fn new(source: ServiceId, dest: ServiceId, bus_id: ServiceBus, request: BusMsg) -> Self {
        PendingRequest {
            source,
            dest,
            bus_id,
            request,
        }
    }
}

impl StrictEncode for PendingRequest {
    fn strict_encode<E: std::io::Write>(&self, mut e: E) -> Result<usize, strict_encoding::Error> {
        let mut len = self.source.strict_encode(&mut e)?;
        len += self.dest.strict_encode(&mut e)?;
        len += self.bus_id.strict_encode(&mut e)?;
        len += self.request.serialize().strict_encode(&mut e)?;
        Ok(len)
    }
}

impl StrictDecode for PendingRequest {
    fn strict_decode<D: std::io::Read>(mut d: D) -> Result<Self, strict_encoding::Error> {
        let unmarshaller: Unmarshaller<BusMsg> = BusMsg::create_unmarshaller();
        let source = ServiceId::strict_decode(&mut d)?;
        let dest = ServiceId::strict_decode(&mut d)?;
        let bus_id = ServiceBus::strict_decode(&mut d)?;
        let request: BusMsg = (&*unmarshaller
            .unmarshall(Cursor::new(Vec::<u8>::strict_decode(&mut d)?))
            .unwrap())
            .clone();
        Ok(PendingRequest {
            source,
            dest,
            bus_id,
            request,
        })
    }
}

#[derive(Debug, Clone, Display, StrictEncode, StrictDecode)]
#[display("checkpoint-swapd")]
pub struct CheckpointSwapd {
    pub state: State,
    pub pending_msg: Option<PeerMsg>,
    pub enquirer: Option<ServiceId>,
    pub xmr_addr_addendum: Option<XmrAddressAddendum>,
    pub temporal_safety: TemporalSafety,
    pub txs: Vec<(TxLabel, bitcoin::Transaction)>,
    pub txids: Vec<(TxLabel, Txid)>,
    pub pending_broadcasts: Vec<bitcoin::Transaction>,
    pub pending_requests: Vec<(ServiceId, Vec<PendingRequest>)>,
    pub local_trade_role: TradeRole,
    pub connected_counterparty_node_id: Option<NodeId>,
    pub public_offer: PublicOffer,
    pub monero_address_creation_height: Option<u64>,
    pub wallet: Wallet,
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
    fn send_peer(&mut self, endpoints: &mut Endpoints, msg: PeerMsg) -> Result<(), Error> {
        trace!("sending peer message {} to {}", msg, self.peer_service);
        if let Err(error) = endpoints.send_to(
            ServiceBus::Msg,
            self.identity(),
            self.peer_service.clone(),
            BusMsg::P2p(msg.clone()),
        ) {
            error!(
                "could not send message {} to {} due to {}",
                msg, self.peer_service, error
            );
            self.connected = false;
            warn!("notifying farcasterd of peer error, farcasterd will attempt to reconnect");
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

    fn swap_id(&self) -> SwapId {
        match self.identity {
            ServiceId::Swap(swap_id) => swap_id,
            _ => {
                unreachable!("not ServiceId::Swap")
            }
        }
    }

    fn pending_requests(&mut self) -> &mut HashMap<ServiceId, Vec<PendingRequest>> {
        &mut self.pending_requests
    }

    fn state_update(&mut self, next_state: State) -> Result<(), Error> {
        info!(
            "{} | State transition: {} -> {}",
            self.swap_id.swap_id(),
            self.state.label(),
            next_state.label(),
        );
        self.state = next_state;
        Ok(())
    }

    fn broadcast(
        &mut self,
        tx: bitcoin::Transaction,
        tx_label: TxLabel,
        endpoints: &mut Endpoints,
    ) -> Result<(), Error> {
        info!(
            "{} | Broadcasting {} tx ({})",
            self.swap_id.swap_id(),
            tx_label.label(),
            tx.txid().tx_hash()
        );
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
            error!("{}", msg);
            return Err(Error::Farcaster(msg));
        }

        if request.swap_id() != self.swap_id() {
            let msg = format!(
                "{} | Incorrect swap_id: expected {}, found {}",
                self.swap_id.bright_blue_italic(),
                self.swap_id(),
                request.swap_id(),
            );
            error!("{}", msg);
            return Err(Error::Farcaster(msg));
        }

        match request {
            // Trade role: Taker, target of this message
            // Potentially the first and only message receive from maker if the offer is not found
            // on maker side.
            PeerMsg::OfferNotFound(swap_id)
                if self.state.commit() && self.state.trade_role() == Some(TradeRole::Taker) =>
            {
                error!(
                    "{} | Taken offer {} was not found by the maker, aborting this swap.",
                    swap_id.swap_id(),
                    self.public_offer.id().swap_id(),
                );
                // just cancel the swap, no additional logic required
                self.state_update(State::Alice(AliceState::FinishA(Outcome::FailureAbort)))?;
                self.abort_swap(endpoints)?;
            }

            // Trade role: Taker, target of this message
            // Message #2 received after Take Swap control message
            //
            // We receive maker (counter-party) commit message, it is now our turn to send the
            // reveal message to counter-party.
            //
            // Upon reception taker needs to
            //  1. If we are playing Bob in the protocol: watch the arbitrating funding address
            //  2. Handle the maker commit message in the wallet
            //  3. Send reveal to the counterparty
            PeerMsg::MakerCommit(remote_commit)
                if self.state.commit()
                    && self.state.trade_role() == Some(TradeRole::Taker)
                    && self.state.remote_commit().is_none() =>
            {
                debug!("{} | received remote maker commitment", self.swap_id);
                self.state.t_sup_remote_commit(remote_commit.clone());
                // if we are Bob watch the arbitrating funding address
                if self.state.swap_role() == SwapRole::Bob {
                    let addr = self
                        .state
                        .b_address()
                        .cloned()
                        .expect("address available at CommitB");
                    debug!("{} | bob, watch arbitrating funding {}", self.swap_id, addr);
                    let txlabel = TxLabel::Funding;
                    if !self.syncer_state.is_watched_addr(&txlabel) {
                        let task = self.syncer_state.watch_addr_btc(addr, txlabel);
                        endpoints.send_to(
                            ServiceBus::Sync,
                            self.identity(),
                            self.syncer_state.bitcoin_syncer(),
                            BusMsg::Sync(SyncMsg::Task(task)),
                        )?;
                    }
                }

                let reveal = self
                    .wallet
                    .as_mut()
                    .expect("should be initialized")
                    .handle_maker_commit(remote_commit, self.swap_id.clone())?;
                debug!(
                    "{} | Wallet handled maker commit and produced reveal",
                    self.swap_id.swap_id()
                );
                self.send_peer(endpoints, PeerMsg::Reveal(reveal))?;
                trace!(
                    "{} | Sent reveal peer message to peerd",
                    self.swap_id.swap_id()
                );
            }

            PeerMsg::TakerCommit(_) => {
                // handled by farcasterd and indirectly here by CtlMsg::MakeSwap
                unreachable!()
            }

            // Trade role: both
            // Message received by counter-party revealing they parameters
            //
            // Upon reception Alice needs to
            //  1. Validate the parameters
            //  2. Handle the Bob Reveal message with the wallet
            //  3. Send the Alice Reveal message to the counterparty
            //
            // Upon reception Bob needs to
            //  1. Validate the parameters
            //  2.1 IF the fee estimation is completed, add the reveal message to the state
            //  2.2 Send the funding information message to farcaster
            //  2.3 Watch the arbitrating funding address if we are maker
            //  3. OR add a pending request for the reveal message
            //
            // Reveal message is received by maker first on his commit state and by taker second on
            // his reveal state. Because taker moves from commit to reveal after sending the reveal
            // message to maker.
            PeerMsg::Reveal(reveal) if self.state.commit() || self.state.reveal() => {
                let remote_commit = self.state.remote_commit().cloned().unwrap();
                if let Ok(validated_params) = validate_reveal(&reveal, remote_commit) {
                    debug!("{} | remote params successfully validated", self.swap_id);
                    self.state.sup_remote_params(validated_params);
                } else {
                    let msg = format!("{} | remote params validation failed", self.swap_id);
                    error!("{}", msg);
                    return Err(Error::Farcaster(msg));
                }

                match self.state.swap_role() {
                    // handle the reveal message with the wallet
                    SwapRole::Alice => {
                        debug!("Alice: handling reveal with wallet");
                        // this should never occur the maker already sent reveal
                        let reveal = self
                            .wallet
                            .as_mut()
                            .expect("should be initialized")
                            .handle_bob_reveals(reveal, self.swap_id.clone())?;

                        if let Some(reveal) = reveal {
                            self.send_peer(endpoints, PeerMsg::Reveal(reveal))?;
                        }
                        let next_state = self.state.clone().sup_commit_to_reveal();
                        self.state_update(next_state)?;
                    }
                    SwapRole::Bob => {
                        if self.state.b_address().is_none() {
                            let msg = format!("{} | bob: address is missing", self.swap_id);
                            error!("{}", msg);
                            return Err(Error::Farcaster(msg));
                        }

                        if let Some(sat_per_kvb) = self.syncer_state.btc_fee_estimate_sat_per_kvb {
                            // 1. transition from Commit to Reveal
                            let next_state = self.state.clone().sup_commit_to_reveal();
                            self.state_update(next_state)?;
                            self.state.b_sup_alice_reveal(reveal.clone());
                            info!("state: {}", self.state);

                            // 2. send the funding information to farcasterd
                            debug!("{} | bob: send funding info to farcasterd", self.swap_id);
                            let address = self.state.b_address().cloned().unwrap();
                            self.ask_bob_to_fund(sat_per_kvb, address.clone(), endpoints)?;
                            let trade_role = self.state.trade_role().unwrap();
                            if trade_role == TradeRole::Maker {
                                // 3. watch the arbitrating address to receive an event on funding
                                debug!("{} | bob: watch address for funding event", self.swap_id);
                                if !self.syncer_state.is_watched_addr(&TxLabel::Funding) {
                                    let watch_addr_task =
                                        self.syncer_state.watch_addr_btc(address, TxLabel::Funding);
                                    endpoints.send_to(
                                        ServiceBus::Sync,
                                        self.identity(),
                                        self.syncer_state.bitcoin_syncer(),
                                        BusMsg::Sync(SyncMsg::Task(watch_addr_task)),
                                    )?;
                                }
                            }
                        } else {
                            // if fee estimate not available yet, defer handling for later
                            debug!("{} | bob: deferring for when fee available", self.swap_id);
                            self.pending_requests.defer_request(
                                self.syncer_state.bitcoin_syncer(),
                                PendingRequest::new(
                                    source,
                                    self.identity(),
                                    ServiceBus::Msg,
                                    BusMsg::P2p(PeerMsg::Reveal(reveal.clone())),
                                ),
                            );
                        }
                    }
                }
            }

            // Swap role: Alice, target of this message
            // Message #1 after commit/reveal
            //
            // The Core Arbitrating Setup is created by Bob and sent to Alice. The message contains
            // the set of transactions used on the Arbitrating blockchain, i.e. Bitcoin in
            // Bitcoin<>Monero.
            //
            // When Alice's swap receives the message from Bob she needs to
            //  1. Register a watch task on syncer for
            //    - Arbitrating lock
            //    - Cancel
            //    - Refund
            //  2. Handle the Core Arbitrating Setup message with her Wallet
            //  3. Recurse on previous confirmation messages for Cancel and Punish
            //  4. Transition to state RefundSigA
            //  5. Checkpoint Alice pre Lock
            //  6. Send RefundProcedureSignature to counterparty
            PeerMsg::CoreArbitratingSetup(setup)
                if self.state.swap_role() == SwapRole::Alice && self.state.reveal() =>
            {
                // register a watch task for arb lock, cancel, and refund
                for (&tx, tx_label) in [&setup.lock, &setup.cancel, &setup.refund].iter().zip([
                    TxLabel::Lock,
                    TxLabel::Cancel,
                    TxLabel::Refund,
                ]) {
                    debug!("{} | register watch {} tx", self.swap_id, tx_label);
                    if !self.syncer_state.is_watched_tx(&tx_label) {
                        let txid = tx.clone().extract_tx().txid();
                        let task = self.syncer_state.watch_tx_btc(txid, tx_label);
                        endpoints.send_to(
                            ServiceBus::Sync,
                            self.identity(),
                            self.syncer_state.bitcoin_syncer(),
                            BusMsg::Sync(SyncMsg::Task(task)),
                        )?;
                    }
                }

                // handle the core arbitrating setup message with the wallet
                debug!("{} | handling core arb setup with wallet", self.swap_id);
                let HandleCoreArbitratingSetupRes {
                    refund_procedure_signatures,
                    cancel_tx,
                    punish_tx,
                } = self
                    .wallet
                    .as_mut()
                    .unwrap()
                    .handle_core_arbitrating_setup(setup.clone(), self.swap_id.clone())?;

                // handle Cancel and Punish transactions
                log_tx_created(self.swap_id, TxLabel::Cancel);
                self.txs.insert(TxLabel::Cancel, cancel_tx);
                log_tx_created(self.swap_id, TxLabel::Punish);
                self.txs.insert(TxLabel::Punish, punish_tx);
                if let Some(lock_tx_confs_req) = self.syncer_state.lock_tx_confs.clone() {
                    self.handle_sync(
                        endpoints,
                        self.syncer_state.bitcoin_syncer(),
                        lock_tx_confs_req,
                    )?;
                }
                if let Some(cancel_tx_confs_req) = self.syncer_state.cancel_tx_confs.clone() {
                    self.handle_sync(
                        endpoints,
                        self.syncer_state.bitcoin_syncer(),
                        cancel_tx_confs_req,
                    )?;
                }

                // checkpoint alice pre lock bob
                if self.state.a_sup_checkpoint_pre_lock() {
                    debug!("{} | checkpointing alice pre lock state", self.swap_id);
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
                        funding_info: None,
                        overfunded: false,
                    });
                    self.state_update(next_state)?;
                    self.checkpoint_state(
                        endpoints,
                        Some(PeerMsg::RefundProcedureSignatures(
                            refund_procedure_signatures.clone(),
                        )),
                    )?;
                }

                // send refund procedure signature message to counter-party
                debug!("{} | send refund proc sig to peer", self.swap_id);
                self.send_peer(
                    endpoints,
                    PeerMsg::RefundProcedureSignatures(refund_procedure_signatures),
                )?;
            }

            // Swap role: Bob, target of this message
            // Message #2 after commit/reveal
            //
            // The Refund Procedure Signature message is created by Alice and sent to Bob. Bob
            // needs this message to safely lock his funds. Upon reception we need to
            //  1. Handle the refund procedure signature with the wallet, extracting the
            //     buy_procedure_signature, lock transaction, cancel transaction, and refund transaction
            //  2. Broadcast the Lock transaction
            //  3. Process params and aggregate xmr address
            //  4. Check if we can broadcast the Cancel or Refund transactions
            //  5. Whatch the buy transaction
            //  6. Add a deferred request for BuyProcedureSignature
            //  7. Checkpoint Bob pre buy
            PeerMsg::RefundProcedureSignatures(refund_proc) if self.state.b_core_arb() => {
                debug!("{} | handling refund proc sig with wallet", self.swap_id);
                self.state.sup_received_refund_procedure_signatures();
                let HandleRefundProcedureSignaturesRes {
                    buy_procedure_signature,
                    lock_tx,
                    cancel_tx,
                    refund_tx,
                } = self
                    .wallet
                    .as_mut()
                    .unwrap()
                    .handle_refund_procedure_signatures(
                        refund_proc.clone(),
                        self.swap_id.clone(),
                    )?;

                // Process and broadcast lock tx
                log_tx_created(self.swap_id, TxLabel::Lock);
                self.broadcast(lock_tx, TxLabel::Lock, endpoints)?;

                // Process params, aggregate and watch xmr address
                if let (Some(Params::Bob(bob_params)), Some(Params::Alice(alice_params))) =
                    (&self.state.local_params(), &self.state.remote_params())
                {
                    let (spend, view) = aggregate_xmr_spend_view(alice_params, bob_params);
                    let txlabel = TxLabel::AccLock;
                    if !self.syncer_state.is_watched_addr(&txlabel) {
                        let task = self.syncer_state.watch_addr_xmr(spend, view, txlabel, None);
                        endpoints.send_to(
                            ServiceBus::Sync,
                            self.identity(),
                            self.syncer_state.monero_syncer(),
                            BusMsg::Sync(SyncMsg::Task(task)),
                        )?
                    }
                } else {
                    error!(
                        "local_params or remote_params not set, state {}",
                        self.state
                    )
                }

                // Handle Cancel and Refund transaction
                log_tx_created(self.swap_id, TxLabel::Cancel);
                log_tx_created(self.swap_id, TxLabel::Refund);
                self.txs.insert(TxLabel::Cancel, cancel_tx);
                self.txs.insert(TxLabel::Refund, refund_tx);
                if let Some(lock_tx_confs_req) = self.syncer_state.lock_tx_confs.clone() {
                    self.handle_sync(
                        endpoints,
                        self.syncer_state.bitcoin_syncer(),
                        lock_tx_confs_req,
                    )?;
                }
                if let Some(cancel_tx_confs_req) = self.syncer_state.cancel_tx_confs.clone() {
                    self.handle_sync(
                        endpoints,
                        self.syncer_state.bitcoin_syncer(),
                        cancel_tx_confs_req,
                    )?;
                }

                if let State::Bob(BobState::CorearbB { buy_proc, .. }) = &mut self.state {
                    *buy_proc = Some(buy_procedure_signature.clone());
                }
                // register a watch task for buy
                debug!("{} | register watch buy tx", self.swap_id);
                if !self.syncer_state.is_watched_tx(&TxLabel::Buy) {
                    let buy_tx = buy_procedure_signature.buy.clone().extract_tx();
                    let task = self.syncer_state.watch_tx_btc(buy_tx.txid(), TxLabel::Buy);
                    endpoints.send_to(
                        ServiceBus::Sync,
                        self.identity(),
                        self.syncer_state.bitcoin_syncer(),
                        BusMsg::Sync(SyncMsg::Task(task)),
                    )?;
                }

                // Send the message to counter-party when predicate is reached later when:
                //  - Arb Lock is final
                //  - Acc Lock is final
                self.pending_requests.defer_request(
                    self.syncer_state.monero_syncer(),
                    PendingRequest::new(
                        self.identity(),
                        self.peer_service.clone(),
                        ServiceBus::Msg,
                        BusMsg::P2p(PeerMsg::BuyProcedureSignature(buy_procedure_signature)),
                    ),
                );

                // Checkpoint Bob pre buy
                if self.state.b_sup_checkpoint_pre_buy() {
                    debug!("{} | checkpointing bob pre buy swapd state", self.swap_id);
                    self.checkpoint_state(endpoints, None)?;
                } else {
                    if !self.state.get_is_checkpoint_pre_buy_ready() {
                        warn!(
                            "{} | trying checkpoint Bob pre buy but incorrect state: {}",
                            self.swap_id, self.state,
                        );
                    }
                }
            }

            // Swap role: Alice, target of this message
            // Message #3 after commit/reveal
            //
            // The Buy Procedure Signature is received by Alice when Bob validated Alice's funding
            // on-chain. This message allows Alice to trigger the swap execution on-chain.
            //
            // Upon reception of this message Alice needs to
            //  1. Register a watch task for the Buy transaction
            //  2. Handle the buy procedure signature by the wallet, extracting Cancel and Buy
            //  3. Check if we can broadcast Cancel or Buy
            //  4. Checkpoint the current swap state
            PeerMsg::BuyProcedureSignature(buy_procedure_signature)
                if self.state.a_refundsig() && !self.state.a_overfunded() =>
            {
                // register a watch task for buy
                debug!("{} | register watch buy tx", self.swap_id);
                if !self.syncer_state.is_watched_tx(&TxLabel::Buy) {
                    let txid = buy_procedure_signature.buy.clone().extract_tx().txid();
                    let task = self.syncer_state.watch_tx_btc(txid, TxLabel::Buy);
                    endpoints.send_to(
                        ServiceBus::Sync,
                        self.identity(),
                        self.syncer_state.bitcoin_syncer(),
                        BusMsg::Sync(SyncMsg::Task(task)),
                    )?;
                }
                // Handle the received buy procedure signature message with the wallet
                debug!("{} | handling buy proc sig with wallet", self.swap_id);
                let HandleBuyProcedureSignatureRes { cancel_tx, buy_tx } = self
                    .wallet
                    .as_mut()
                    .unwrap()
                    .handle_buy_procedure_signature(
                        buy_procedure_signature,
                        self.swap_id.clone(),
                    )?;

                // Handle Cancel and Buy transactions
                log_tx_created(self.swap_id, TxLabel::Cancel);
                log_tx_created(self.swap_id, TxLabel::Buy);
                self.txs.insert(TxLabel::Cancel, cancel_tx);
                self.txs.insert(TxLabel::Buy, buy_tx);
                if let Some(lock_tx_confs_req) = self.syncer_state.lock_tx_confs.clone() {
                    self.handle_sync(
                        endpoints,
                        self.syncer_state.bitcoin_syncer(),
                        lock_tx_confs_req,
                    )?;
                }
                if let Some(cancel_tx_confs_req) = self.syncer_state.cancel_tx_confs.clone() {
                    self.handle_sync(
                        endpoints,
                        self.syncer_state.bitcoin_syncer(),
                        cancel_tx_confs_req,
                    )?;
                }

                // checkpoint swap alice pre buy
                if self.state.a_sup_checkpoint_pre_buy() {
                    debug!("{} | checkpointing alice pre buy swapd state", self.swap_id);
                    self.checkpoint_state(endpoints, None)?;
                }
            }

            // bob and alice
            PeerMsg::Abort(_) => {
                return Err(Error::Farcaster("Abort not yet supported".to_string()))
            }

            PeerMsg::Ping(_) | PeerMsg::Pong(_) | PeerMsg::PingPeer => {
                unreachable!("ping/pong must remain in peerd, and unreachable in swapd")
            }

            req => error!(
                "{} | BusMsg {} not supported at state {} on MSG interface",
                self.swap_id.swap_id(),
                req,
                self.state
            ),
        }

        Ok(())
    }

    fn handle_ctl(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: CtlMsg,
    ) -> Result<(), Error> {
        match request {
            CtlMsg::Hello => {
                info!(
                    "{} | Service {} daemon is now {}",
                    self.swap_id.swap_id(),
                    source.bright_green_bold(),
                    "connected"
                );
            }
            CtlMsg::Terminate if source == ServiceId::Farcasterd => {
                info!(
                    "{} | {}",
                    self.swap_id.swap_id(),
                    format!("Terminating {}", self.identity()).label()
                );
                std::process::exit(0);
            }

            CtlMsg::Disconnected => {
                self.connected = false;
            }

            CtlMsg::Reconnected => {
                self.connected = true;
            }

            // Trade role: Taker, target of this message
            // Message sent by farcasterd upon reception of TakeOffer message
            //
            // First message received on the taker swap side.
            //
            // Upon reception of this message Taker needs to
            //  1. Start the fee estimation process
            //  2. Subscribe to blockchain height change events
            //  3. Send the peer-to-peer Taker Commit message to counter-party
            CtlMsg::TakeSwap(InitTakerSwap {
                peerd,
                report_to,
                swap_id,
                key_manager,
                target_bitcoin_address,
                target_monero_address,
            }) if self.state.start() => {
                if ServiceId::Swap(swap_id) != self.identity {
                    error!(
                        "{}: {}",
                        "This swapd instance is not reponsible for swap_id", swap_id
                    );
                    return Ok(());
                };
                // start fee estimation and block height changes
                self.syncer_state.watch_fee_and_height(endpoints)?;
                self.peer_service = peerd;
                if let ServiceId::Peer(0, _) = self.peer_service {
                    self.connected = false;
                } else {
                    self.connected = true;
                }

                self.enquirer = Some(report_to.clone());

                let wallet = Wallet::new_taker(
                    endpoints,
                    self.public_offer.clone(),
                    target_bitcoin_address,
                    target_monero_address,
                    key_manager.0,
                    swap_id,
                )?;

                let local_params = wallet.local_params();
                let funding_address = wallet.funding_address();
                self.wallet = Some(wallet);

                let local_commit =
                    self.taker_commit(endpoints, local_params.clone())
                        .map_err(|err| {
                            error!("{}", err);
                            self.report_failure_to(
                                endpoints,
                                &self.enquirer.clone(),
                                Failure {
                                    code: FailureCode::Unknown,
                                    info: err.to_string(),
                                },
                            )
                        })?;
                let next_state = self.state.clone().sup_start_to_commit(
                    local_commit.clone(),
                    local_params,
                    funding_address,
                    None,
                );
                let take_swap = TakerCommit {
                    commit: local_commit,
                    public_offer: self.public_offer.clone(),
                };
                // send taker commit message to counter-party
                self.send_peer(endpoints, PeerMsg::TakerCommit(take_swap))?;
                self.state_update(next_state)?;
            }

            // Trade role: Maker, target of this message
            // Message sent by farcasterd upon reception of p2p TakerCommit message
            //
            // First message received by swapd on the maker side that initiate the swap protocol.
            //
            // Upon reception of this message Maker needs to
            //  1. Start watching fee and height
            //  2. Create commitments
            //  3. Make a transition to state Commit
            //  4. Send the peer-to-peer Maker Commit message to counter-party
            // The first message received on the maker swap side.
            CtlMsg::MakeSwap(InitMakerSwap {
                peerd,
                report_to,
                key_manager,
                swap_id,
                target_bitcoin_address,
                target_monero_address,
                commit,
            }) if self.state.start() => {
                // start fee estimation and block height changes
                self.syncer_state.watch_fee_and_height(endpoints)?;

                let wallet = Wallet::new_maker(
                    endpoints,
                    self.public_offer.clone(),
                    target_bitcoin_address,
                    target_monero_address,
                    key_manager.0,
                    swap_id,
                    commit.clone(),
                )?;

                let local_params = wallet.local_params();
                let funding_address = wallet.funding_address();

                self.wallet = Some(wallet);

                self.peer_service = peerd;
                if self.peer_service != ServiceId::Loopback {
                    self.connected = true;
                }
                self.enquirer = Some(report_to.clone());
                let local_commit = self
                    .maker_commit(endpoints, swap_id, local_params.clone())
                    .map_err(|err| {
                        self.report_failure_to(
                            endpoints,
                            &self.enquirer.clone(),
                            Failure {
                                code: FailureCode::Unknown,
                                info: err.to_string(),
                            },
                        )
                    })?;
                let next_state = self.state.clone().sup_start_to_commit(
                    local_commit.clone(),
                    local_params,
                    funding_address,
                    Some(commit),
                );
                // send maker commit message to counter-party
                trace!("sending peer MakerCommit msg {}", &local_commit);
                self.send_peer(endpoints, PeerMsg::MakerCommit(local_commit))?;
                self.state_update(next_state)?;
            }

            CtlMsg::AbortSwap
                if self.state.a_start()
                    || self.state.a_commit()
                    || self.state.a_reveal()
                    || (self.state.a_refundsig() && !self.state.a_btc_locked()) =>
            {
                // just cancel the swap, no additional logic required
                self.state_update(State::Alice(AliceState::FinishA(Outcome::FailureAbort)))?;
                self.abort_swap(endpoints)?;
                self.send_client_info(
                    endpoints,
                    source,
                    InfoMsg::String("Aborted swap".to_string()),
                )?;
            }

            CtlMsg::AbortSwap if self.state.b_start() => {
                // just cancel the swap, no additional logic required, since funding was not yet retrieved
                self.state_update(State::Bob(BobState::FinishB(Outcome::FailureAbort)))?;
                self.abort_swap(endpoints)?;
                self.send_client_info(
                    endpoints,
                    source,
                    InfoMsg::String("Aborted swap".to_string()),
                )?;
            }

            CtlMsg::AbortSwap
                if self.state.b_commit()
                    || self.state.b_reveal()
                    || (!self.state.b_received_refund_procedure_signatures()
                        && self.state.b_core_arb()) =>
            {
                let sweep_btc = self
                    .wallet
                    .as_mut()
                    .unwrap()
                    .process_get_sweep_bitcoin_address(
                        self.state.b_address().cloned().unwrap(),
                        self.swap_id.clone(),
                    )?;
                info!(
                    "{} | Sweeping source (funding) address: {} to destination address: {}",
                    self.swap_id.swap_id(),
                    sweep_btc.source_address.addr(),
                    sweep_btc.destination_address.addr()
                );
                let task = self.syncer_state.sweep_btc(sweep_btc, false);
                endpoints.send_to(
                    ServiceBus::Sync,
                    self.identity(),
                    self.syncer_state.bitcoin_syncer(),
                    BusMsg::Sync(SyncMsg::Task(task)),
                )?;

                // cancel the swap to invalidate its state
                self.state_update(State::Bob(BobState::FinishB(Outcome::FailureAbort)))?;
                self.send_client_info(
                    endpoints,
                    source,
                    InfoMsg::String("Aborting swap, checking if funds can be sweeped.".to_string()),
                )?;
            }

            CtlMsg::AbortSwap => {
                let msg = "Swap is already locked-in, cannot manually abort anymore.".to_string();
                warn!("{} | {}", self.swap_id.swap_id(), msg);

                self.send_client_ctl(
                    endpoints,
                    source,
                    CtlMsg::Failure(Failure {
                        code: FailureCode::Unknown,
                        info: msg,
                    }),
                )?;
            }

            // Set the reconnected service id. This can happen if this is a
            // maker launched swap after restoration and the taker reconnects,
            // after a manual connect call, or a new connection with the same
            // node address is established
            CtlMsg::PeerdReconnected(service_id) => {
                info!("{} | Peer {} reconnected", self.swap_id, service_id);
                self.peer_service = service_id.clone();
                self.connected = true;
                self.pending_requests
                    .update_deferred_requests_peer_destination(service_id);
                for msg in self.pending_peer_request.clone().iter() {
                    self.send_peer(endpoints, msg.clone())?;
                }
                self.pending_peer_request.clear();
            }

            CtlMsg::FailedPeerMessage(msg) => {
                warn!(
                    "{} | Sending the peer message {} failed. Adding to pending peer requests",
                    self.swap_id, msg
                );
                self.pending_peer_request.push(msg);
            }

            CtlMsg::Checkpoint(Checkpoint { swap_id, state }) => {
                let CheckpointSwapd {
                    state,
                    pending_msg,
                    enquirer,
                    temporal_safety,
                    mut txs,
                    txids,
                    mut pending_requests,
                    pending_broadcasts,
                    xmr_addr_addendum,
                    local_trade_role,
                    wallet,
                    monero_address_creation_height,
                    ..
                } = state;
                info!("{} | Restoring swap", swap_id.swap_id());
                self.state = state;
                self.wallet = Some(wallet);
                self.enquirer = enquirer;
                self.temporal_safety = temporal_safety;
                self.pending_requests = pending_requests.drain(..).collect();
                self.monero_address_creation_height = monero_address_creation_height;
                // We need to update the peerd for the pending requests in case of reconnect
                self.pending_requests
                    .update_deferred_requests_peer_destination(self.peer_service.clone());
                self.local_trade_role = local_trade_role;
                self.txs = txs.drain(..).collect();
                trace!("Watch height bitcoin");
                let watch_height_bitcoin = self.syncer_state.watch_height(Blockchain::Bitcoin);
                endpoints.send_to(
                    ServiceBus::Sync,
                    self.identity(),
                    self.syncer_state.bitcoin_syncer(),
                    BusMsg::Sync(SyncMsg::Task(watch_height_bitcoin)),
                )?;

                trace!("Watch height monero");
                let watch_height_monero = self.syncer_state.watch_height(Blockchain::Monero);
                endpoints.send_to(
                    ServiceBus::Sync,
                    self.identity(),
                    self.syncer_state.monero_syncer(),
                    BusMsg::Sync(SyncMsg::Task(watch_height_monero)),
                )?;

                trace!("Watching transactions");
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

                trace!("broadcasting txs pending broadcast");
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

                let msg = format!("Restored swap at state {}", self.state);
                let _ = self.report_progress_message_to(endpoints, ServiceId::Farcasterd, msg);
            }

            req => {
                error!(
                    "BusMsg {} is not supported by the CTL interface",
                    req.to_string()
                );
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
                let swap_id = if self.swap_id() == zero!() {
                    None
                } else {
                    Some(self.swap_id())
                };
                let connection = self.peer_service.node_addr();
                let info = SwapInfo {
                    swap_id,
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
                    public_offer: self.public_offer.clone(),
                    local_trade_role: self.local_trade_role,
                    local_swap_role: self.public_offer.swap_role(&self.local_trade_role),
                    connected_counterparty_node_id: get_node_id(&self.peer_service),
                };
                self.send_client_info(endpoints, source, InfoMsg::SwapInfo(info))?;
            }

            req => {
                error!(
                    "BusMsg {} is not supported by the INFO interface",
                    req.to_string()
                );
            }
        }

        Ok(())
    }

    fn handle_sync(
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

                    Event::AddressTransaction(AddressTransaction {
                        id,
                        hash,
                        amount,
                        block,
                        tx,
                    }) if self.state.swap_role() == SwapRole::Alice
                        && self.syncer_state.tasks.watched_addrs.contains_key(id)
                        && !self.state.a_xmr_locked()
                        && self.syncer_state.tasks.watched_addrs.get(id).unwrap()
                            == &TxLabel::AccLock =>
                    {
                        debug!(
                            "Event details: {} {:?} {} {:?} {:?}",
                            id, hash, amount, block, tx
                        );
                        self.state.a_sup_refundsig_xmrlocked();

                        let required_funding_amount = self
                            .state
                            .a_required_funding_amount()
                            .expect("set when monero funding address is displayed");
                        if amount.clone() < required_funding_amount {
                            // Alice still views underfunding as valid in the hope that Bob still passes her BuyProcSig
                            let msg = format!(
                                "Too small amount funded. Required: {}, Funded: {}. Do not fund this swap anymore, will attempt to refund.",
                                monero::Amount::from_pico(required_funding_amount),
                                monero::Amount::from_pico(amount.clone())
                            );
                            error!("{}", msg);
                            self.report_progress_message_to(endpoints, self.enquirer.clone(), msg)?;
                        } else if amount.clone() > required_funding_amount {
                            // Alice set overfunded to ensure that she does not publish the buy transaction if Bob gives her the BuySig.
                            self.state.a_sup_overfunded();
                            let msg = format!(
                                "Too big amount funded. Required: {}, Funded: {}. Do not fund this swap anymore, will attempt to refund.",
                                monero::Amount::from_pico(required_funding_amount),
                                monero::Amount::from_pico(amount.clone())
                            );
                            error!("{}", msg);
                            self.report_progress_message_to(endpoints, self.enquirer.clone(), msg)?;
                        }

                        let txlabel = TxLabel::AccLock;
                        if !self.syncer_state.is_watched_tx(&txlabel) {
                            if self.syncer_state.awaiting_funding {
                                endpoints.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    ServiceId::Farcasterd,
                                    BusMsg::Ctl(CtlMsg::FundingCompleted(Blockchain::Monero)),
                                )?;
                                self.syncer_state.awaiting_funding = false;
                            }
                            let task = self.syncer_state.watch_tx_xmr(hash.clone(), txlabel);
                            endpoints.send_to(
                                ServiceBus::Sync,
                                self.identity(),
                                self.syncer_state.monero_syncer(),
                                BusMsg::Sync(SyncMsg::Task(task)),
                            )?;
                        }
                        if self.syncer_state.tasks.watched_addrs.remove(id).is_some() {
                            let abort_task = self.syncer_state.abort_task(*id);
                            endpoints.send_to(
                                ServiceBus::Sync,
                                self.identity(),
                                self.syncer_state.monero_syncer(),
                                BusMsg::Sync(SyncMsg::Task(abort_task)),
                            )?;
                        }
                    }

                    Event::AddressTransaction(AddressTransaction {
                        id,
                        hash,
                        amount,
                        block: _,
                        tx: _,
                    }) if self.state.swap_role() == SwapRole::Bob
                        && self.syncer_state.tasks.watched_addrs.contains_key(id)
                        && self.syncer_state.is_watched_addr(&TxLabel::AccLock)
                        && self.syncer_state.tasks.watched_addrs.get(id).unwrap()
                            == &TxLabel::AccLock =>
                    {
                        let amount = monero::Amount::from_pico(*amount);
                        if amount < self.syncer_state.monero_amount {
                            warn!(
                                "Not enough monero locked: expected {}, found {}",
                                self.syncer_state.monero_amount, amount
                            );
                            return Ok(());
                        }
                        if let Some(tx_label) = self.syncer_state.tasks.watched_addrs.remove(id) {
                            if !self.syncer_state.is_watched_tx(&tx_label) {
                                let watch_tx =
                                    self.syncer_state.watch_tx_xmr(hash.clone(), tx_label);
                                endpoints.send_to(
                                    ServiceBus::Sync,
                                    self.identity(),
                                    self.syncer_state.monero_syncer(),
                                    BusMsg::Sync(SyncMsg::Task(watch_tx)),
                                )?;
                            }
                            let abort_task = self.syncer_state.abort_task(*id);
                            endpoints.send_to(
                                ServiceBus::Sync,
                                self.identity(),
                                self.syncer_state.monero_syncer(),
                                BusMsg::Sync(SyncMsg::Task(abort_task)),
                            )?;
                        }
                    }

                    Event::TransactionConfirmations(TransactionConfirmations {
                        confirmations: Some(confirmations),
                        ..
                    }) if (self.state.b_buy_sig()
                        || (self.state.a_refundsig() && self.state.a_xmr_locked()))
                        && *confirmations >= self.temporal_safety.sweep_monero_thr
                        && self.pending_requests().contains_key(&source)
                        && self
                            .pending_requests()
                            .get(&source)
                            .map(|r| r.len() > 0)
                            .unwrap_or(false) =>
                    {
                        let PendingRequest {
                            source: _,
                            request,
                            dest,
                            bus_id,
                        } = self
                            .pending_requests()
                            .remove(&source)
                            .expect("Checked above")
                            .pop()
                            .unwrap();
                        if let (
                            BusMsg::Sync(SyncMsg::Task(Task::SweepAddress(task))),
                            ServiceBus::Sync,
                        ) = (request.clone(), bus_id)
                        {
                            let request = BusMsg::Sync(SyncMsg::Task(Task::SweepAddress(task)));

                            info!(
                                "{} | Monero are spendable now (height {}), sweeping ephemeral wallet",
                                self.swap_id.swap_id(),
                                self.syncer_state.monero_height.label()
                            );
                            endpoints.send_to(bus_id, self.identity(), dest, request)?;
                        } else {
                            error!(
                                "Not the sweep task {} or not Ctl bus found {}",
                                request, bus_id
                            );
                        }
                    }

                    Event::TransactionConfirmations(TransactionConfirmations {
                        confirmations: Some(confirmations),
                        ..
                    }) if self
                        .temporal_safety
                        .final_tx(*confirmations, Blockchain::Monero)
                        && self.state.b_core_arb()
                        && !self.state.cancel_seen()
                        && self.pending_requests().contains_key(&source)
                        && self
                            .pending_requests()
                            .get(&source)
                            .map(|reqs| reqs.len() == 1)
                            .unwrap() =>
                    {
                        // error!("not checking tx rcvd is accordant lock");
                        let success = PendingRequests::continue_deferred_requests(
                            self,
                            endpoints,
                            source,
                            |r| {
                                matches!(
                                    r,
                                    &PendingRequest {
                                        bus_id: ServiceBus::Msg,
                                        request: BusMsg::P2p(PeerMsg::BuyProcedureSignature(_)),
                                        ..
                                    }
                                )
                            },
                        );
                        if success {
                            let next_state = State::Bob(BobState::BuySigB {
                                buy_tx_seen: false,
                                last_checkpoint_type: self.state.last_checkpoint_type().unwrap(),
                            });
                            self.state_update(next_state)?;
                        }
                    }

                    Event::TransactionConfirmations(TransactionConfirmations {
                        id,
                        confirmations,
                        ..
                    }) if self.syncer_state.tasks.watched_txs.contains_key(id)
                        && !self
                            .temporal_safety
                            .final_tx(confirmations.unwrap_or(0), Blockchain::Monero) =>
                    {
                        self.syncer_state.handle_tx_confs(
                            id,
                            confirmations,
                            self.swap_id(),
                            self.temporal_safety.xmr_finality_thr,
                        );
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

                    Event::TaskAborted(_) => {}

                    Event::SweepSuccess(SweepSuccess { id, .. })
                        if (self.state.b_buy_sig() || self.state.a_xmr_locked())
                            && self.syncer_state.tasks.sweeping_addr.is_some()
                            && &self.syncer_state.tasks.sweeping_addr.unwrap() == id =>
                    {
                        if self.syncer_state.awaiting_funding {
                            warn!(
                                "FundingCompleted never emitted, but not possible to sweep\
                                   monero without passing through funding completed:\
                                   emitting it now to clean up farcasterd"
                            );
                            self.syncer_state.awaiting_funding = false;
                            match self.state.swap_role() {
                                SwapRole::Alice => {
                                    endpoints.send_to(
                                        ServiceBus::Ctl,
                                        self.identity(),
                                        ServiceId::Farcasterd,
                                        BusMsg::Ctl(CtlMsg::FundingCompleted(Blockchain::Monero)),
                                    )?;
                                }
                                SwapRole::Bob => {
                                    endpoints.send_to(
                                        ServiceBus::Ctl,
                                        self.identity(),
                                        ServiceId::Farcasterd,
                                        BusMsg::Ctl(CtlMsg::FundingCompleted(Blockchain::Bitcoin)),
                                    )?;
                                }
                            }
                        }
                        let abort_all = Task::Abort(Abort {
                            task_target: TaskTarget::AllTasks,
                            respond: Boolean::False,
                        });
                        endpoints.send_to(
                            ServiceBus::Sync,
                            self.identity(),
                            self.syncer_state.monero_syncer(),
                            BusMsg::Sync(SyncMsg::Task(abort_all.clone())),
                        )?;
                        endpoints.send_to(
                            ServiceBus::Sync,
                            self.identity(),
                            self.syncer_state.bitcoin_syncer(),
                            BusMsg::Sync(SyncMsg::Task(abort_all)),
                        )?;
                        let success = if self.state.b_buy_sig() {
                            self.state_update(State::Bob(BobState::FinishB(Outcome::SuccessSwap)))?;
                            Some(Outcome::SuccessSwap)
                        } else if self.state.a_refund_seen() {
                            self.state_update(State::Alice(AliceState::FinishA(
                                Outcome::FailureRefund,
                            )))?;
                            Some(Outcome::FailureRefund)
                        } else {
                            error!("Unexpected sweeping state, not sending finalization commands to farcasterd");
                            None
                        };
                        if let Some(success) = success {
                            let swap_success_req = BusMsg::Ctl(CtlMsg::SwapOutcome(success));
                            self.send_ctl(endpoints, ServiceId::Farcasterd, swap_success_req)?;
                            // remove txs to invalidate outdated states
                            self.txs.remove(&TxLabel::Cancel);
                            self.txs.remove(&TxLabel::Refund);
                            self.txs.remove(&TxLabel::Buy);
                            self.txs.remove(&TxLabel::Punish);
                        }
                    }
                    Event::Empty(id) => {
                        debug!("task with id {} has produced no events yet", id);
                        if self.state.swap_role() == SwapRole::Alice
                            && self.syncer_state.tasks.watched_addrs.contains_key(id)
                            && !self.state.a_xmr_locked()
                            && self.syncer_state.tasks.watched_addrs.get(id).unwrap()
                                == &TxLabel::AccLock
                        {
                            if let Some(monero_funding_info) = self.state.a_funding_info() {
                                info!(
                                    "{} | Send {} to {}",
                                    self.swap_id.bright_blue_italic(),
                                    monero_funding_info.amount.bright_green_bold(),
                                    monero_funding_info.address.addr(),
                                );
                                let funding_request = BusMsg::Ctl(CtlMsg::FundingInfo(
                                    FundingInfo::Monero(monero_funding_info),
                                ));
                                self.syncer_state.awaiting_funding = true;
                                if let Some(enquirer) = self.enquirer.clone() {
                                    endpoints.send_to(
                                        ServiceBus::Ctl,
                                        self.identity(),
                                        enquirer,
                                        funding_request,
                                    )?
                                }
                            } else {
                                warn!("Expected alice's state to contain existing monero funding info");
                            }
                        } else {
                            debug!("Received Empty event with unknown purpose");
                        }
                    }

                    event => {
                        error!("event not handled {}", event)
                    }
                }
            }

            SyncMsg::Event(ref event) if source == self.syncer_state.bitcoin_syncer => {
                match &event {
                    Event::HeightChanged(HeightChanged { height, .. }) => {
                        self.syncer_state
                            .handle_height_change(*height, Blockchain::Bitcoin);
                    }

                    Event::AddressTransaction(AddressTransaction { id, amount, tx, .. })
                        if self.syncer_state.tasks.watched_addrs.get(id).is_some() =>
                    {
                        let tx = bitcoin::Transaction::deserialize(
                            &tx.into_iter().flatten().copied().collect::<Vec<u8>>(),
                        )?;
                        info!(
                            "Received AddressTransaction, processing tx {}",
                            &tx.txid().tx_hash()
                        );
                        let txlabel = self.syncer_state.tasks.watched_addrs.get(id).unwrap();
                        match txlabel {
                            TxLabel::Funding
                                if self.syncer_state.awaiting_funding
                                    && self.state.b_required_funding_amount().is_some() =>
                            {
                                log_tx_seen(self.swap_id, txlabel, &tx.txid());
                                self.syncer_state.awaiting_funding = false;
                                // If the bitcoin amount does not match the expected funding amount, abort the swap
                                let amount = bitcoin::Amount::from_sat(*amount);
                                let required_funding_amount =
                                    self.state.b_required_funding_amount().unwrap();

                                if amount != required_funding_amount {
                                    // incorrect funding, start aborting procedure
                                    let msg = format!("Incorrect amount funded. Required: {}, Funded: {}. Do not fund this swap anymore, will abort and atttempt to sweep the Bitcoin to the provided address.", amount, required_funding_amount);
                                    error!("{}", msg);
                                    self.report_progress_message_to(
                                        endpoints,
                                        ServiceId::Farcasterd,
                                        msg,
                                    )?;
                                    // FIXME: syncer shall not have permission to AbortSwap, replace source by identity?
                                    self.handle_ctl(endpoints, source, CtlMsg::AbortSwap)?;
                                    return Ok(());
                                } else {
                                    // funding completed, amount is correct
                                    endpoints.send_to(
                                        ServiceBus::Ctl,
                                        self.identity(),
                                        ServiceId::Farcasterd,
                                        BusMsg::Ctl(CtlMsg::FundingCompleted(Blockchain::Bitcoin)),
                                    )?;
                                }

                                // process tx with wallet
                                self.wallet
                                    .as_mut()
                                    .unwrap()
                                    .process_funding_tx(Tx::Funding(tx), self.swap_id)?;

                                let (reveal, core_arb_setup) =
                                    self.wallet.as_mut().unwrap().handle_alice_reveals(
                                        self.state.get_alice_reveal().unwrap(),
                                        self.swap_id.clone(),
                                    )?;

                                if let Some(reveal) = reveal {
                                    debug!(
                                        "{} | Sending Bob reveal to Alice",
                                        self.swap_id.swap_id()
                                    );
                                    self.send_peer(endpoints, PeerMsg::Reveal(reveal))?;
                                }

                                // register a watch task for arb lock, cancel, and refund
                                for (&tx, tx_label) in [
                                    &core_arb_setup.lock,
                                    &core_arb_setup.cancel,
                                    &core_arb_setup.refund,
                                ]
                                .iter()
                                .zip([
                                    TxLabel::Lock,
                                    TxLabel::Cancel,
                                    TxLabel::Refund,
                                ]) {
                                    debug!("{} | register watch {} tx", self.swap_id, tx_label);
                                    if !self.syncer_state.is_watched_tx(&tx_label) {
                                        let txid = tx.clone().extract_tx().txid();
                                        let task = self.syncer_state.watch_tx_btc(txid, tx_label);
                                        endpoints.send_to(
                                            ServiceBus::Sync,
                                            self.identity(),
                                            self.syncer_state.bitcoin_syncer(),
                                            BusMsg::Sync(SyncMsg::Task(task)),
                                        )?;
                                    }
                                }

                                // Set the monero address creation height for Bob before setting the first checkpoint
                                if self.monero_address_creation_height.is_none() {
                                    self.monero_address_creation_height =
                                        Some(self.syncer_state.height(Blockchain::Monero));
                                }

                                // checkpoint swap pre lock bob
                                if self.state.b_sup_checkpoint_pre_lock() {
                                    debug!("{} | checkpointing bob pre lock state", self.swap_id);
                                    // transition to new state
                                    let next_state = State::Bob(BobState::CorearbB {
                                        received_refund_procedure_signatures: false,
                                        local_params: self.state.local_params().cloned().unwrap(),
                                        cancel_seen: false,
                                        remote_params: self.state.remote_params().unwrap(),
                                        b_address: self.state.b_address().cloned().unwrap(),
                                        last_checkpoint_type: self
                                            .state
                                            .last_checkpoint_type()
                                            .unwrap(),
                                        buy_tx_seen: false,
                                        buy_proc: None,
                                    });
                                    self.state_update(next_state)?;
                                    self.checkpoint_state(
                                        endpoints,
                                        Some(PeerMsg::CoreArbitratingSetup(core_arb_setup.clone())),
                                    )?;
                                }

                                // send the message to counter-party
                                debug!("{} | send core arb setup to peer", self.swap_id);
                                self.send_peer(
                                    endpoints,
                                    PeerMsg::CoreArbitratingSetup(core_arb_setup.clone()),
                                )?;
                            }

                            txlabel => {
                                error!(
                                    "address transaction event not supported for tx {} at state {}",
                                    txlabel, &self.state
                                )
                            }
                        }
                    }

                    Event::AddressTransaction(AddressTransaction { tx, .. }) => {
                        let tx = bitcoin::Transaction::deserialize(
                            &tx.into_iter().flatten().copied().collect::<Vec<u8>>(),
                        )?;
                        warn!(
                            "unknown address transaction with txid {}",
                            &tx.txid().addr()
                        )
                    }

                    Event::TransactionRetrieved(TransactionRetrieved { id, tx: Some(tx) })
                        if self.syncer_state.tasks.retrieving_txs.contains_key(id) =>
                    {
                        let (txlabel, _) =
                            self.syncer_state.tasks.retrieving_txs.remove(id).unwrap();
                        match txlabel {
                            TxLabel::Buy if self.state.b_buy_sig() => {
                                log_tx_seen(self.swap_id, &txlabel, &tx.txid());
                                self.state.b_sup_buy_tx_seen();
                                let sweep_xmr = self.wallet.as_mut().unwrap().process_buy_tx(
                                    tx.clone(),
                                    endpoints,
                                    self.swap_id.clone(),
                                    self.monero_address_creation_height,
                                )?;
                                let task = self.syncer_state.sweep_xmr(sweep_xmr.clone(), true);
                                let acc_confs_needs = self
                                    .syncer_state
                                    .get_confs(TxLabel::AccLock)
                                    .map(|c| {
                                        self.temporal_safety
                                            .sweep_monero_thr
                                            .checked_sub(c)
                                            .unwrap_or(0)
                                    })
                                    .unwrap_or(self.temporal_safety.sweep_monero_thr);
                                let sweep_block = self.syncer_state.height(Blockchain::Monero)
                                    + acc_confs_needs as u64;
                                info!(
                                    "{} | Tx {} needs {} confirmations, and has {} confirmations",
                                    self.swap_id.swap_id(),
                                    TxLabel::AccLock.label(),
                                    acc_confs_needs.bright_green_bold(),
                                    self.syncer_state.get_confs(TxLabel::AccLock).unwrap_or(0),
                                );
                                info!(
                                    "{} | {} reaches your address {} after block {}",
                                    self.swap_id.swap_id(),
                                    Blockchain::Monero.label(),
                                    sweep_xmr.destination_address.addr(),
                                    sweep_block.bright_blue_bold(),
                                );
                                warn!(
                                    "Peerd might crash, just ignore it, counterparty closed \
                                           connection but you don't need it anymore!"
                                );
                                self.pending_requests.defer_request(
                                    self.syncer_state.monero_syncer(),
                                    PendingRequest::new(
                                        self.identity(),
                                        self.syncer_state.monero_syncer(),
                                        ServiceBus::Sync,
                                        BusMsg::Sync(SyncMsg::Task(task)),
                                    ),
                                );
                            }
                            TxLabel::Buy if self.state.b_core_arb() => {
                                log_tx_seen(self.swap_id, &txlabel, &tx.txid());
                                self.state.b_sup_buy_tx_seen();
                                let sweep_xmr = self.wallet.as_mut().unwrap().process_buy_tx(
                                    tx.clone(),
                                    endpoints,
                                    self.swap_id.clone(),
                                    self.monero_address_creation_height,
                                )?;
                                let task = self.syncer_state.sweep_xmr(sweep_xmr.clone(), true);
                                let acc_confs_needs = self
                                    .syncer_state
                                    .get_confs(TxLabel::AccLock)
                                    .map(|c| {
                                        self.temporal_safety
                                            .sweep_monero_thr
                                            .checked_sub(c)
                                            .unwrap_or(0)
                                    })
                                    .unwrap_or(self.temporal_safety.sweep_monero_thr);
                                let sweep_block = self.syncer_state.height(Blockchain::Monero)
                                    + acc_confs_needs as u64;
                                info!(
                                    "{} | Tx {} needs {} confirmations, and has {} confirmations",
                                    self.swap_id.swap_id(),
                                    TxLabel::AccLock.label(),
                                    acc_confs_needs.bright_green_bold(),
                                    self.syncer_state.get_confs(TxLabel::AccLock).unwrap_or(0),
                                );
                                info!(
                                    "{} | {} reaches your address {} after block {}",
                                    self.swap_id.swap_id(),
                                    Blockchain::Monero.label(),
                                    sweep_xmr.destination_address.addr(),
                                    sweep_block.bright_blue_bold(),
                                );
                                warn!(
                                    "Peerd might crash, just ignore it, counterparty closed \
                                           connection but you don't need it anymore!"
                                );
                                self.pending_requests.defer_request(
                                    self.syncer_state.monero_syncer(),
                                    PendingRequest::new(
                                        self.identity(),
                                        self.syncer_state.monero_syncer(),
                                        ServiceBus::Sync,
                                        BusMsg::Sync(SyncMsg::Task(task)),
                                    ),
                                );

                                // The buy transaction is received in Corearb state, go straight to buy state
                                let next_state = State::Bob(BobState::BuySigB {
                                    buy_tx_seen: false,
                                    last_checkpoint_type: self
                                        .state
                                        .last_checkpoint_type()
                                        .unwrap(),
                                });
                                self.state_update(next_state)?;
                            }
                            TxLabel::Buy => {
                                warn!(
                                    "expected BobState(BuySigB), found {}. Any chance you reused the \
                                     destination/refund address in the cli command? For your own privacy, \
                                     do not reuse bitcoin addresses. Txid {}",
                                    self.state,
                                    tx.txid().addr(),
                                )
                            }
                            TxLabel::Refund
                                if self.state.a_refundsig() && self.state.a_xmr_locked() =>
                            {
                                log_tx_seen(self.swap_id, &txlabel, &tx.txid());
                                let sweep_xmr = self.wallet.as_mut().unwrap().process_refund_tx(
                                    endpoints,
                                    tx.clone(),
                                    self.swap_id.clone(),
                                    self.monero_address_creation_height,
                                )?;
                                let task = self.syncer_state.sweep_xmr(sweep_xmr.clone(), true);
                                let acc_confs_needs = self
                                    .syncer_state
                                    .get_confs(TxLabel::AccLock)
                                    .map(|c| {
                                        self.temporal_safety
                                            .sweep_monero_thr
                                            .checked_sub(c)
                                            .unwrap_or(0)
                                    })
                                    .unwrap_or(self.temporal_safety.sweep_monero_thr);
                                let sweep_block = self.syncer_state.height(Blockchain::Monero)
                                    + acc_confs_needs as u64;
                                info!(
                                    "{} | Tx {} needs {} confirmations, and has {} confirmations",
                                    self.swap_id.swap_id(),
                                    TxLabel::AccLock.label(),
                                    acc_confs_needs.bright_green_bold(),
                                    self.syncer_state.get_confs(TxLabel::AccLock).unwrap_or(0),
                                );
                                info!(
                                    "{} | {} reaches your address {} after block {}",
                                    self.swap_id.swap_id(),
                                    Blockchain::Monero.label(),
                                    sweep_xmr.destination_address.addr(),
                                    sweep_block.bright_blue_bold(),
                                );
                                warn!(
                                    "Peerd might crash, just ignore it, counterparty closed\
                                           connection but you don't need it anymore!"
                                );
                                self.pending_requests.defer_request(
                                    self.syncer_state.monero_syncer(),
                                    PendingRequest::new(
                                        self.identity(),
                                        self.syncer_state.monero_syncer(),
                                        ServiceBus::Sync,
                                        BusMsg::Sync(SyncMsg::Task(task)),
                                    ),
                                );
                            }
                            txlabel => {
                                error!(
                                    "Transaction retrieved event not supported for tx {} at state {}",
                                    txlabel, &self.state
                                )
                            }
                        }
                    }

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
                        let txlabel = self.syncer_state.tasks.watched_txs.get(id).unwrap();
                        // saving requests of interest for later replaying latest event
                        // TODO MAYBE: refactor this block into following TxLabel match as an outer block with inner matching again
                        match &txlabel {
                            TxLabel::Lock => {
                                self.syncer_state.lock_tx_confs = Some(request.clone());
                            }
                            TxLabel::Cancel => {
                                self.syncer_state.cancel_tx_confs = Some(request.clone());
                                self.state.sup_cancel_seen();
                            }

                            _ => {}
                        }
                        match txlabel {
                            TxLabel::Funding => {}
                            TxLabel::Lock
                                if self.state.a_refundsig()
                                    && !self.state.a_xmr_locked()
                                    && !self.state.a_buy_published()
                                    && self.state.local_params().is_some()
                                    && self.state.remote_params().is_some()
                                    && !self.syncer_state.acc_lock_watched() =>
                            {
                                self.state.a_sup_refundsig_btclocked();
                                // TODO: implement state management here?
                                if let (
                                    Some(Params::Alice(alice_params)),
                                    Some(Params::Bob(bob_params)),
                                ) = (&self.state.local_params(), &self.state.remote_params())
                                {
                                    let (spend, view) =
                                        aggregate_xmr_spend_view(alice_params, bob_params);
                                    // Set the monero address creation height for Alice right after the first aggregation
                                    if self.monero_address_creation_height.is_none() {
                                        self.monero_address_creation_height =
                                            Some(self.syncer_state.height(Blockchain::Monero));
                                    }
                                    let viewpair = monero::ViewPair { spend, view };
                                    let address = monero::Address::from_viewpair(
                                        self.syncer_state.network.into(),
                                        &viewpair,
                                    );
                                    let swap_id = self.swap_id();
                                    let amount = self.syncer_state.monero_amount;
                                    self.state.a_sup_required_funding_amount(amount);
                                    self.state.a_sup_funding_info(Some(MoneroFundingInfo {
                                        swap_id,
                                        address,
                                        amount,
                                    }));

                                    let txlabel = TxLabel::AccLock;
                                    if !self.syncer_state.is_watched_addr(&txlabel) {
                                        let watch_addr_task = self.syncer_state.watch_addr_xmr(
                                            spend,
                                            view,
                                            txlabel,
                                            self.monero_address_creation_height,
                                        );
                                        endpoints.send_to(
                                            ServiceBus::Sync,
                                            self.identity(),
                                            self.syncer_state.monero_syncer(),
                                            BusMsg::Sync(SyncMsg::Task(watch_addr_task)),
                                        )?;
                                    }
                                } else {
                                    error!(
                                        "local_params or remote_params not set for Alice, state {}",
                                        self.state
                                    )
                                }
                            }
                            TxLabel::Lock
                                if self.temporal_safety.valid_cancel(*confirmations)
                                    && self.state.safe_cancel()
                                    && self.txs.contains_key(&TxLabel::Cancel) =>
                            {
                                let (tx_label, cancel_tx) =
                                    self.txs.remove_entry(&TxLabel::Cancel).unwrap();
                                self.broadcast(cancel_tx, tx_label, endpoints)?
                            }
                            TxLabel::Lock
                                if self.temporal_safety.safe_buy(*confirmations)
                                    && self.state.swap_role() == SwapRole::Alice
                                    && self.state.a_refundsig()
                                    && !self.state.a_buy_published()
                                    && !self.state.cancel_seen()
                                    && !self.state.a_overfunded() // don't publish buy in case we overfunded
                                    && self.txs.contains_key(&TxLabel::Buy)
                                    && self.state.remote_params().is_some()
                                    && self.state.local_params().is_some() =>
                            {
                                let xmr_locked = self.state.a_xmr_locked();
                                let btc_locked = self.state.a_btc_locked();
                                let overfunded = self.state.a_overfunded();
                                let required_funding_amount =
                                    self.state.a_required_funding_amount();
                                if let Some((txlabel, buy_tx)) =
                                    self.txs.remove_entry(&TxLabel::Buy)
                                {
                                    self.broadcast(buy_tx, txlabel, endpoints)?;
                                    self.state = State::Alice(AliceState::RefundSigA {
                                        local_params: self.state.local_params().cloned().unwrap(),
                                        buy_published: true, // FIXME
                                        btc_locked,
                                        xmr_locked,
                                        cancel_seen: false,
                                        refund_seen: false,
                                        remote_params: self.state.remote_params().unwrap(),
                                        last_checkpoint_type: self
                                            .state
                                            .last_checkpoint_type()
                                            .unwrap(),
                                        funding_info: None,
                                        required_funding_amount,
                                        overfunded,
                                    });
                                } else {
                                    warn!(
                                        "Alice doesn't have the buy tx, probably didnt receive \
                                             the buy signature yet: {}",
                                        self.state
                                    );
                                }
                            }
                            TxLabel::Lock
                                if self
                                    .temporal_safety
                                    .stop_funding_before_cancel(*confirmations)
                                    && self.state.safe_cancel()
                                    && self.state.swap_role() == SwapRole::Alice
                                    && self.syncer_state.awaiting_funding =>
                            {
                                warn!(
                                    "{} | Alice, the swap may be cancelled soon. Do not fund anymore",
                                    self.swap_id.swap_id()
                                );
                                self.syncer_state.awaiting_funding = false;
                                endpoints.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    ServiceId::Farcasterd,
                                    BusMsg::Ctl(CtlMsg::FundingCanceled(Blockchain::Monero)),
                                )?
                            }

                            TxLabel::Cancel
                                if self.temporal_safety.valid_punish(*confirmations)
                                    && self.state.a_refundsig()
                                    && self.state.a_xmr_locked()
                                    && self.txs.contains_key(&TxLabel::Punish)
                                    && !self.state.a_refund_seen() =>
                            {
                                trace!("Alice publishes punish tx");

                                let (tx_label, punish_tx) =
                                    self.txs.remove_entry(&TxLabel::Punish).unwrap();
                                // syncer's watch punish tx task
                                if !self.syncer_state.is_watched_tx(&tx_label) {
                                    let txid = punish_tx.txid();
                                    let task = self.syncer_state.watch_tx_btc(txid, tx_label);
                                    endpoints.send_to(
                                        ServiceBus::Sync,
                                        self.identity(),
                                        self.syncer_state.bitcoin_syncer(),
                                        BusMsg::Sync(SyncMsg::Task(task)),
                                    )?;
                                }

                                self.broadcast(punish_tx, tx_label, endpoints)?;
                            }

                            TxLabel::Cancel
                                if self
                                    .temporal_safety
                                    .final_tx(*confirmations, Blockchain::Bitcoin)
                                    && (self.state.b_buy_sig() || self.state.b_core_arb())
                                    && self.txs.contains_key(&TxLabel::Refund) =>
                            {
                                trace!("here Bob publishes refund tx");
                                if !self.temporal_safety.safe_refund(*confirmations) {
                                    warn!("Publishing refund tx, but we might already have been punished");
                                }
                                let (tx_label, refund_tx) =
                                    self.txs.remove_entry(&TxLabel::Refund).unwrap();
                                self.broadcast(refund_tx, tx_label, endpoints)?;
                            }
                            TxLabel::Cancel
                                if (self.state.swap_role() == SwapRole::Alice
                                    && !self.state.a_xmr_locked()) =>
                            {
                                warn!(
                                    "{} | Alice, this swap was canceled. Do not fund anymore.",
                                    self.swap_id.swap_id()
                                );
                                if self.syncer_state.awaiting_funding {
                                    endpoints.send_to(
                                        ServiceBus::Ctl,
                                        self.identity(),
                                        ServiceId::Farcasterd,
                                        BusMsg::Ctl(CtlMsg::FundingCanceled(Blockchain::Monero)),
                                    )?;
                                    self.syncer_state.awaiting_funding = false;
                                }
                                self.state_update(State::Alice(AliceState::FinishA(
                                    Outcome::FailureRefund,
                                )))?;
                                let abort_all = Task::Abort(Abort {
                                    task_target: TaskTarget::AllTasks,
                                    respond: Boolean::False,
                                });
                                endpoints.send_to(
                                    ServiceBus::Sync,
                                    self.identity(),
                                    self.syncer_state.monero_syncer(),
                                    BusMsg::Sync(SyncMsg::Task(abort_all.clone())),
                                )?;
                                endpoints.send_to(
                                    ServiceBus::Sync,
                                    self.identity(),
                                    self.syncer_state.bitcoin_syncer(),
                                    BusMsg::Sync(SyncMsg::Task(abort_all)),
                                )?;
                                let swap_success_req =
                                    BusMsg::Ctl(CtlMsg::SwapOutcome(Outcome::FailureRefund));
                                self.send_ctl(endpoints, ServiceId::Farcasterd, swap_success_req)?;
                                self.txs.remove(&TxLabel::Buy);
                                self.txs.remove(&TxLabel::Cancel);
                                self.txs.remove(&TxLabel::Punish);
                            }
                            TxLabel::Buy
                                if self
                                    .temporal_safety
                                    .final_tx(*confirmations, Blockchain::Bitcoin)
                                    && self.state.a_refundsig() =>
                            {
                                // Swap ends here for alice
                                self.state_update(State::Alice(AliceState::FinishA(
                                    Outcome::SuccessSwap,
                                )))?;
                                let abort_all = Task::Abort(Abort {
                                    task_target: TaskTarget::AllTasks,
                                    respond: Boolean::False,
                                });
                                endpoints.send_to(
                                    ServiceBus::Sync,
                                    self.identity(),
                                    self.syncer_state.monero_syncer(),
                                    BusMsg::Sync(SyncMsg::Task(abort_all.clone())),
                                )?;
                                endpoints.send_to(
                                    ServiceBus::Sync,
                                    self.identity(),
                                    self.syncer_state.bitcoin_syncer(),
                                    BusMsg::Sync(SyncMsg::Task(abort_all)),
                                )?;
                                let swap_success_req =
                                    BusMsg::Ctl(CtlMsg::SwapOutcome(Outcome::SuccessSwap));
                                self.send_ctl(endpoints, ServiceId::Farcasterd, swap_success_req)?;
                                self.txs.remove(&TxLabel::Cancel);
                                self.txs.remove(&TxLabel::Punish);
                            }
                            TxLabel::Buy
                                if self.state.swap_role() == SwapRole::Bob
                                    && self.syncer_state.tasks.txids.contains_key(txlabel) =>
                            {
                                debug!("request Buy tx task");
                                let (txlabel, txid) =
                                    self.syncer_state.tasks.txids.remove_entry(txlabel).unwrap();
                                let task = self.syncer_state.retrieve_tx_btc(txid, txlabel);
                                endpoints.send_to(
                                    ServiceBus::Sync,
                                    self.identity(),
                                    self.syncer_state.bitcoin_syncer(),
                                    BusMsg::Sync(SyncMsg::Task(task)),
                                )?;
                            }
                            TxLabel::Refund
                                if self.state.swap_role() == SwapRole::Alice
                                    && !self.state.a_refund_seen()
                                    && self.syncer_state.tasks.txids.contains_key(txlabel) =>
                            {
                                debug!("subscribe Refund address task");
                                self.state.a_sup_refundsig_refund_seen();
                                let (txlabel, txid) =
                                    self.syncer_state.tasks.txids.remove_entry(txlabel).unwrap();
                                let task = self.syncer_state.retrieve_tx_btc(txid, txlabel);
                                endpoints.send_to(
                                    ServiceBus::Sync,
                                    self.identity(),
                                    self.syncer_state.bitcoin_syncer(),
                                    BusMsg::Sync(SyncMsg::Task(task)),
                                )?;
                            }

                            TxLabel::Refund if self.state.swap_role() == SwapRole::Bob => {
                                let abort_all = Task::Abort(Abort {
                                    task_target: TaskTarget::AllTasks,
                                    respond: Boolean::False,
                                });
                                endpoints.send_to(
                                    ServiceBus::Sync,
                                    self.identity(),
                                    self.syncer_state.monero_syncer(),
                                    BusMsg::Sync(SyncMsg::Task(abort_all.clone())),
                                )?;
                                endpoints.send_to(
                                    ServiceBus::Sync,
                                    self.identity(),
                                    self.syncer_state.bitcoin_syncer(),
                                    BusMsg::Sync(SyncMsg::Task(abort_all)),
                                )?;
                                self.state_update(State::Bob(BobState::FinishB(
                                    Outcome::FailureRefund,
                                )))?;
                                let swap_success_req =
                                    BusMsg::Ctl(CtlMsg::SwapOutcome(Outcome::FailureRefund));
                                self.send_ctl(endpoints, ServiceId::Farcasterd, swap_success_req)?;
                                // remove txs to invalidate outdated states
                                self.txs.remove(&TxLabel::Cancel);
                                self.txs.remove(&TxLabel::Refund);
                                self.txs.remove(&TxLabel::Buy);
                                self.txs.remove(&TxLabel::Punish);
                            }
                            TxLabel::Punish => {
                                let abort_all = Task::Abort(Abort {
                                    task_target: TaskTarget::AllTasks,
                                    respond: Boolean::False,
                                });
                                endpoints.send_to(
                                    ServiceBus::Sync,
                                    self.identity(),
                                    self.syncer_state.monero_syncer(),
                                    BusMsg::Sync(SyncMsg::Task(abort_all.clone())),
                                )?;
                                endpoints.send_to(
                                    ServiceBus::Sync,
                                    self.identity(),
                                    self.syncer_state.bitcoin_syncer(),
                                    BusMsg::Sync(SyncMsg::Task(abort_all)),
                                )?;
                                match self.state.swap_role() {
                                    SwapRole::Alice => self.state_update(State::Alice(
                                        AliceState::FinishA(Outcome::FailurePunish),
                                    ))?,
                                    SwapRole::Bob => {
                                        warn!("{}", "You were punished!".err());
                                        self.state_update(State::Bob(BobState::FinishB(
                                            Outcome::FailurePunish,
                                        )))?
                                    }
                                }
                                let swap_success_req =
                                    BusMsg::Ctl(CtlMsg::SwapOutcome(Outcome::FailurePunish));
                                self.send_ctl(endpoints, ServiceId::Farcasterd, swap_success_req)?;
                                // remove txs to invalidate outdated states
                                self.txs.remove(&TxLabel::Cancel);
                                self.txs.remove(&TxLabel::Refund);
                                self.txs.remove(&TxLabel::Buy);
                                self.txs.remove(&TxLabel::Punish);
                            }
                            tx_label => trace!(
                                "{} | Tx {} with {} confirmations evokes no response in state {}",
                                self.swap_id,
                                tx_label,
                                confirmations,
                                &self.state
                            ),
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

                    Event::TaskAborted(event) => {
                        debug!("{}", event)
                    }

                    Event::SweepSuccess(SweepSuccess { id, .. })
                        if self.state.b_outcome_abort()
                            && self.syncer_state.tasks.sweeping_addr.is_some()
                            && &self.syncer_state.tasks.sweeping_addr.unwrap() == id =>
                    {
                        self.state_update(State::Bob(BobState::FinishB(Outcome::FailureAbort)))?;
                        endpoints.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            ServiceId::Farcasterd,
                            BusMsg::Ctl(CtlMsg::FundingCanceled(Blockchain::Bitcoin)),
                        )?;
                        self.abort_swap(endpoints)?;
                    }

                    Event::SweepSuccess(event) => {
                        debug!("{}", event)
                    }

                    Event::TransactionRetrieved(event) => {
                        debug!("{}", event)
                    }

                    Event::AddressBalance(event) => {
                        debug!("{}", event)
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
                        info!("Fee: {} sat/kvB", high_priority_sats_per_kvbyte);
                        self.syncer_state.btc_fee_estimate_sat_per_kvb =
                            Some(*high_priority_sats_per_kvbyte);

                        if self.state.remote_commit().is_some()
                            && (self.state.commit() || self.state.reveal())
                            && self.state.b_address().is_some()
                            && self.syncer_state.btc_fee_estimate_sat_per_kvb.is_some()
                        {
                            let success = PendingRequests::continue_deferred_requests(
                                self,
                                endpoints,
                                source,
                                |i| {
                                    matches!(
                                        &i,
                                        &PendingRequest {
                                            bus_id: ServiceBus::Msg,
                                            request: BusMsg::P2p(PeerMsg::Reveal(_)),
                                            dest: ServiceId::Swap(..),
                                            ..
                                        }
                                    )
                                },
                            );
                            if success {
                                debug!("successfully dispatched reveal:aliceparams")
                            } else {
                                debug!("failed dispatching reveal:aliceparams, maybe sent before")
                            }
                        }
                    }
                    Event::Empty(_) => debug!("empty event not handled for Bitcoin"),

                    Event::HealthResult(_) => {
                        debug!("ignoring health result in swapd")
                    }
                }
            }

            req => {
                if let SyncMsg::Event(Event::TransactionConfirmations(confs)) = req.clone() {
                    info!("confs: {}", confs);
                }
                info!("source: {}", source);
                info!("{:?}", self.syncer_state.tasks.watched_txs);
                info!("{}", self.syncer_state.is_watched_tx(&TxLabel::Lock));
                info!("Lock info: safe_buy: {}, swap_role_alice: {}, a_refundsig: {}, not buy published: {}, not cancel seen {}, not overfunded {}, buy exists {}, remote_params {}, local params {}",
                self.temporal_safety.safe_buy(2)
                , self.state.swap_role() == SwapRole::Alice
                , self.state.a_refundsig()
                , !self.state.a_buy_published()
                , !self.state.cancel_seen()
                , !self.state.a_overfunded() // don't publish buy in case we overfunded
                , self.txs.contains_key(&TxLabel::Buy)
                , self.state.remote_params().is_some()
                , self.state.local_params().is_some());
                error!("BusMsg {} is not supported by the SYNC interface", req);
            }
        }

        Ok(())
    }
}

impl Runtime {
    fn report_potential_state_change(&mut self, endpoints: &mut Endpoints) -> Result<(), Error> {
        // Generate a new state report for the clients
        let new_state_report =
            StateReport::new(&self.state, &self.temporal_safety, &self.syncer_state);
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
            swap_id.swap_id(),
            amount.bright_green_bold(),
            address.addr(),
            total_fees.label(),
        );
        self.state.b_sup_required_funding_amount(amount);
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
        Ok(())
    }
    pub fn taker_commit(
        &mut self,
        endpoints: &mut Endpoints,
        params: Params,
    ) -> Result<Commit, Error> {
        info!(
            "{} | {} to Maker remote peer",
            self.swap_id().swap_id(),
            "Proposing to take swap".bright_white_bold(),
        );

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
        info!(
            "{} | {} as Maker from Taker through peerd {}",
            swap_id.swap_id(),
            "Accepting swap".bright_white_bold(),
            self.peer_service.bright_blue_italic()
        );

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

    fn abort_swap(&mut self, endpoints: &mut Endpoints) -> Result<(), Error> {
        let swap_success_req = BusMsg::Ctl(CtlMsg::SwapOutcome(Outcome::FailureAbort));
        self.send_ctl(endpoints, ServiceId::Farcasterd, swap_success_req)?;
        info!("{} | Aborted swap.", self.swap_id.swap_id());
        Ok(())
    }

    fn checkpoint_state(
        &mut self,
        endpoints: &mut Endpoints,
        pending_msg: Option<PeerMsg>,
    ) -> Result<(), Error> {
        endpoints.send_to(
            ServiceBus::Ctl,
            self.identity(),
            ServiceId::Database,
            BusMsg::Ctl(CtlMsg::Checkpoint(Checkpoint {
                swap_id: self.swap_id,
                state: CheckpointSwapd {
                    state: self.state.clone(),
                    pending_msg,
                    enquirer: self.enquirer.clone(),
                    temporal_safety: self.temporal_safety.clone(),
                    txs: self.txs.clone().drain().collect(),
                    txids: self.syncer_state.tasks.txids.clone().drain().collect(),
                    pending_requests: self.pending_requests().clone().drain().collect(),
                    pending_broadcasts: self.syncer_state.pending_broadcast_txs(),
                    xmr_addr_addendum: self.syncer_state.xmr_addr_addendum.clone(),
                    local_trade_role: self.local_trade_role,
                    connected_counterparty_node_id: get_node_id(&self.peer_service),
                    public_offer: self.public_offer.clone(),
                    wallet: self.wallet.as_mut().unwrap().clone(),
                    monero_address_creation_height: self.monero_address_creation_height.clone(),
                },
            })),
        )?;
        Ok(())
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

fn aggregate_xmr_spend_view(
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
fn validate_reveal(reveal: &Reveal, remote_commit: Commit) -> Result<Params, Error> {
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
