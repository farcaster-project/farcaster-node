// LNP Node: node running lightning network protocol and generalized lightning
// channels.
// Written in 2020 by
//     Dr. Maxim Orlovsky <orlovsky@pandoracore.com>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to

// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the MIT License
// along with this software.
// If not, see <https://opensource.org/licenses/MIT>.

use super::{
    swap_state::{AliceState, BobState, State, SwapCheckpointType},
    syncer_client::{log_tx_received, log_tx_seen, SyncerState, SyncerTasks},
    temporal_safety::TemporalSafety,
};
use crate::databased::checkpoint_send;
use crate::service::Endpoints;
use crate::syncerd::bitcoin_syncer::p2wpkh_signed_tx_fee;
use crate::syncerd::types::{AddressTransaction, Boolean, Event, Task, TransactionConfirmations};
use crate::syncerd::{FeeEstimation, FeeEstimations, SweepAddressAddendum};
use crate::{
    bus::ctl::{
        BitcoinFundingInfo, Checkpoint, CheckpointState, Ctl, FundingInfo, InitSwap,
        MoneroFundingInfo, Params, Tx,
    },
    bus::msg::{Commit, Msg, Reveal, TakeCommit},
    bus::rpc::{Rpc, SwapInfo},
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
    swap::btcxmr::{message::CoreArbitratingSetup, Offer, Parameters, PublicOffer},
    swap::SwapId,
    transaction::TxLabel,
};
use internet2::{addr::NodeAddr, CreateUnmarshaller, TypedEnum, Unmarshall, Unmarshaller};
use microservices::esb::{self, Handler};
use strict_encoding::{StrictDecode, StrictEncode};

pub fn run(
    config: ServiceConfig,
    swap_id: SwapId,
    public_offer: PublicOffer,
    local_trade_role: TradeRole,
) -> Result<(), Error> {
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
    let sweep_monero_thr = 10;
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
        btc_finality_thr: 1,
        race_thr: 3,
        xmr_finality_thr: 1,
        sweep_monero_thr,
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
    };

    let runtime = Runtime {
        swap_id,
        identity: ServiceId::Swap(swap_id),
        peer_service: ServiceId::Loopback,
        state: init_state,
        maker_peer: None,
        started: SystemTime::now(),
        syncer_state,
        temporal_safety,
        enquirer: None,
        pending_requests: none!(),
        pending_peer_request: none!(),
        txs: none!(),
        public_offer,
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
    state: State,
    maker_peer: Option<NodeAddr>,
    started: SystemTime,
    enquirer: Option<ServiceId>,
    syncer_state: SyncerState,
    temporal_safety: TemporalSafety,
    pending_requests: PendingRequests,
    pending_peer_request: Vec<Msg>, // Peer requests that failed and are waiting for reconnection
    txs: HashMap<TxLabel, bitcoin::Transaction>,
    public_offer: PublicOffer,
}

// FIXME Something more meaningful than ServiceId to index
type PendingRequests = HashMap<ServiceId, Vec<PendingRequest>>;

impl PendingRequestsT for PendingRequests {
    fn defer_request(&mut self, key: ServiceId, pending_req: PendingRequest) {
        let pending_reqs = self.entry(key).or_insert(vec![]);
        pending_reqs.push(pending_req);
    }
    fn continue_deferred_requests(
        runtime: &mut Runtime,
        endpoints: &mut Endpoints,
        key: ServiceId,
        predicate: fn(&PendingRequest) -> bool,
    ) -> bool {
        let success = if let Some(pending_reqs) = runtime.pending_requests.remove(&key) {
            let len0 = pending_reqs.len();
            let remaining_pending_reqs: Vec<_> =
                pending_reqs
                    .into_iter()
                    .filter_map(|r| {
                        if predicate(&r) {
                            if let Ok(_) = match (&r.bus_id, &r.request) {
                                (ServiceBus::Ctl, _) if &r.dest == &runtime.identity => runtime
                                    .handle_ctl(endpoints, r.source.clone(), r.request.clone()),
                                (ServiceBus::Msg, _) if &r.dest == &runtime.identity => runtime
                                    .handle_msg(endpoints, r.source.clone(), r.request.clone()),
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

#[derive(Debug, Clone, Display)]
#[display("checkpoint-swapd")]
pub struct CheckpointSwapd {
    pub state: State,
    pub last_msg: Msg,
    pub enquirer: Option<ServiceId>,
    pub xmr_addr_addendum: Option<XmrAddressAddendum>,
    pub temporal_safety: TemporalSafety,
    pub txs: HashMap<TxLabel, bitcoin::Transaction>,
    pub txids: HashMap<TxLabel, Txid>,
    pub pending_broadcasts: Vec<bitcoin::Transaction>,
    pub pending_requests: HashMap<ServiceId, Vec<PendingRequest>>,
}

impl StrictEncode for CheckpointSwapd {
    fn strict_encode<E: std::io::Write>(&self, mut e: E) -> Result<usize, strict_encoding::Error> {
        let mut len = self.state.strict_encode(&mut e)?;
        len += self.last_msg.strict_encode(&mut e)?;
        len += self.enquirer.strict_encode(&mut e)?;
        len += self.xmr_addr_addendum.strict_encode(&mut e)?;
        len += self.temporal_safety.strict_encode(&mut e)?;
        len += self.pending_broadcasts.strict_encode(&mut e)?;

        len += self.txs.len().strict_encode(&mut e)?;
        let res: Result<usize, strict_encoding::Error> =
            self.txs.iter().try_fold(len, |mut acc, (key, val)| {
                acc += key.strict_encode(&mut e).map_err(|err| {
                    strict_encoding::Error::DataIntegrityError(format!("{}", err))
                })?;
                acc += val.strict_encode(&mut e).map_err(|err| {
                    strict_encoding::Error::DataIntegrityError(format!("{}", err))
                })?;
                Ok(acc)
            });
        len = match res {
            Ok(val) => Ok(val),
            Err(err) => Err(strict_encoding::Error::DataIntegrityError(format!(
                "{}",
                err
            ))),
        }?;

        len += self.txids.len().strict_encode(&mut e)?;
        let res: Result<usize, strict_encoding::Error> =
            self.txids.iter().try_fold(len, |mut acc, (key, val)| {
                acc += key.strict_encode(&mut e).map_err(|err| {
                    strict_encoding::Error::DataIntegrityError(format!("{}", err))
                })?;
                acc += val.strict_encode(&mut e).map_err(|err| {
                    strict_encoding::Error::DataIntegrityError(format!("{}", err))
                })?;
                Ok(acc)
            });
        len = match res {
            Ok(val) => Ok(val),
            Err(err) => Err(strict_encoding::Error::DataIntegrityError(format!(
                "{}",
                err
            ))),
        }?;

        len += self.pending_requests.len().strict_encode(&mut e)?;
        self.pending_requests
            .iter()
            .try_fold(len, |mut acc, (key, val)| {
                acc += key.strict_encode(&mut e)?;
                acc += val.strict_encode(&mut e)?;
                Ok(acc)
            })
    }
}

impl StrictDecode for CheckpointSwapd {
    fn strict_decode<D: std::io::Read>(mut d: D) -> Result<Self, strict_encoding::Error> {
        let state = State::strict_decode(&mut d)?;
        let last_msg = Msg::strict_decode(&mut d)?;
        let enquirer = Option::<ServiceId>::strict_decode(&mut d)?;
        let xmr_addr_addendum = Option::<XmrAddressAddendum>::strict_decode(&mut d)?;
        let temporal_safety = TemporalSafety::strict_decode(&mut d)?;
        let pending_broadcasts = Vec::<bitcoin::Transaction>::strict_decode(&mut d)?;

        let len = usize::strict_decode(&mut d)?;
        let mut txs = HashMap::<TxLabel, bitcoin::Transaction>::new();
        for _ in 0..len {
            let key = TxLabel::strict_decode(&mut d)?;
            let val = bitcoin::Transaction::strict_decode(&mut d)?;
            if txs.contains_key(&key) {
                return Err(strict_encoding::Error::RepeatedValue(format!("{:?}", key)));
            }
            txs.insert(key, val);
        }

        let len = usize::strict_decode(&mut d)?;
        let mut txids = HashMap::<TxLabel, Txid>::new();
        for _ in 0..len {
            let key = TxLabel::strict_decode(&mut d)?;
            let val = Txid::strict_decode(&mut d)?;
            if txids.contains_key(&key) {
                return Err(strict_encoding::Error::RepeatedValue(format!("{:?}", key)));
            }
            txids.insert(key, val);
        }

        let len = usize::strict_decode(&mut d)?;
        let mut pending_requests = HashMap::<ServiceId, Vec<PendingRequest>>::new();
        for _ in 0..len {
            let key = ServiceId::strict_decode(&mut d)?;
            let val = Vec::<PendingRequest>::strict_decode(&mut d)?;
            if pending_requests.contains_key(&key) {
                return Err(strict_encoding::Error::RepeatedValue(format!("{:?}", key)));
            }
            pending_requests.insert(key, val);
        }
        Ok(CheckpointSwapd {
            state,
            last_msg,
            enquirer,
            xmr_addr_addendum,
            temporal_safety,
            txs,
            txids,
            pending_requests,
            pending_broadcasts,
        })
    }
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
            // Peer-to-peer message bus
            (ServiceBus::Msg, request) => self.handle_msg(endpoints, source, request),
            // Control bus for issuing control commands
            (ServiceBus::Ctl, request) => self.handle_ctl(endpoints, source, request),
            // RPC command bus, only accept BusMsg::Rpc
            (ServiceBus::Rpc, BusMsg::Rpc(req)) => self.handle_rpc(endpoints, source, req),
            // Syncer event bus for blockchain tasks and events, only accept BusMsg::Sync
            (ServiceBus::Sync, BusMsg::Sync(req)) => self.handle_sync(endpoints, source, req),
            // All other pairs are not supported
            (_, request) => Err(Error::NotSupported(bus, request.to_string())),
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
    fn send_peer(&mut self, endpoints: &mut Endpoints, msg: Msg) -> Result<(), Error> {
        trace!("sending peer message {} to {}", msg, self.peer_service);
        if let Err(error) = endpoints.send_to(
            ServiceBus::Msg,
            self.identity(),
            self.peer_service.clone(), // ServiceId::Loopback if not initiailized
            BusMsg::Msg(msg.clone()),
        ) {
            error!(
                "could not send message {} to {} due to {}",
                msg, self.peer_service, error
            );
            warn!("notifying farcasterd of peer error, farcasterd will attempt to reconnect");
            endpoints.send_to(
                ServiceBus::Ctl,
                self.identity(),
                ServiceId::Farcasterd,
                BusMsg::Ctl(Ctl::PeerdUnreachable(self.peer_service.clone())),
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

    fn state_update(&mut self, endpoints: &mut Endpoints, next_state: State) -> Result<(), Error> {
        info!(
            "{} | State transition: {} -> {}",
            self.swap_id.swap_id(),
            self.state.label(),
            next_state.label(),
        );
        let msg = format!("{} -> {}", self.state, next_state,);
        self.state = next_state;
        self.report_state_transition_progress_message_to(endpoints, self.enquirer.clone(), msg)?;
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
        request: BusMsg,
    ) -> Result<(), Error> {
        if self.peer_service != source {
            return Err(Error::Farcaster(format!(
                "{}: expected {}, found {}",
                "Incorrect peer connection", self.peer_service, source
            )));
        }
        let msg = match &request {
            BusMsg::Msg(msg) => {
                if msg.swap_id() != self.swap_id() {
                    return Err(Error::Farcaster(format!(
                        "{}: expected {}, found {}",
                        "Incorrect swap_id ",
                        self.swap_id(),
                        msg.swap_id(),
                    )));
                } else {
                    msg
                }
            }
            _ => {
                error!("MSG RPC can be only used for forwarding farcaster protocol messages");
                return Err(Error::NotSupported(ServiceBus::Msg, request.to_string()));
            }
        };
        let msg_bus = ServiceBus::Msg;
        match &msg {
            // we are taker and the maker committed, now we reveal after checking
            // whether we're Bob or Alice and that we're on a compatible state
            Msg::MakerCommit(remote_commit)
                if self.state.commit()
                    && self.state.trade_role() == Some(TradeRole::Taker)
                    && self.state.remote_commit().is_none() =>
            {
                trace!("received remote commitment");
                self.state.t_sup_remote_commit(remote_commit.clone());

                if self.state.swap_role() == SwapRole::Bob {
                    let addr = self
                        .state
                        .b_address()
                        .cloned()
                        .expect("address available at CommitB");
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

                self.send_wallet(msg_bus, endpoints, request)?;
            }
            Msg::TakerCommit(_) => {
                unreachable!(
                    "msg handled by farcasterd/walletd, and indirectly here by \
                             Ctl BusMsg::MakeSwap"
                )
            }
            Msg::Reveal(Reveal::Proof(_)) => {
                // These messages are saved as pending if Bob and then forwarded once the
                // parameter reveal forward is triggered. If Alice, send immediately.
                match self.state.swap_role() {
                    SwapRole::Bob => {
                        let pending_request = PendingRequest::new(
                            self.identity(),
                            ServiceId::Wallet,
                            ServiceBus::Msg,
                            request,
                        );

                        self.pending_requests
                            .defer_request(ServiceId::Wallet, pending_request);
                    }
                    SwapRole::Alice => {
                        debug!("Alice: forwarding reveal");
                        trace!(
                            "sending request {} to {} on bus {}",
                            &request,
                            &ServiceId::Wallet,
                            &ServiceBus::Msg
                        );
                        self.send_wallet(msg_bus, endpoints, request)?
                    }
                }
            }
            Msg::Reveal(Reveal::AliceParameters(..))
                if self.state.swap_role() == SwapRole::Bob
                    && (self.state.b_address().is_none()
                        || self.syncer_state.btc_fee_estimate_sat_per_kvb.is_none()) =>
            {
                if self.state.b_address().is_none() {
                    let msg = format!("FIXME: b_address is None, request {}", request);
                    error!("{}", msg);
                    return Err(Error::Farcaster(msg));
                }
                debug!(
                    "Deferring request {} for when btc_fee_estimate available, then recurse in the runtime",
                    &request
                );
                let pending_req = PendingRequest::new(source, self.identity(), msg_bus, request);
                self.pending_requests
                    .defer_request(self.syncer_state.bitcoin_syncer(), pending_req);
            }
            // bob and alice
            // store parameters from counterparty if we have not received them yet.
            // if we're maker, also reveal to taker if their commitment is valid.
            Msg::Reveal(reveal)
                if self.state.remote_commit().is_some()
                    && (self.state.commit() || self.state.reveal())
                    && {
                        match (
                            self.state.swap_role(),
                            self.state.b_address().is_some(),
                            self.syncer_state.btc_fee_estimate_sat_per_kvb.is_some(),
                        ) {
                            (SwapRole::Bob, true, true) => true,
                            (SwapRole::Bob, ..) => false,
                            (SwapRole::Alice, ..) => true,
                        }
                    } =>
            {
                // TODO: since we're not actually revealing, find other name for
                // intermediary state

                let remote_commit = self.state.remote_commit().cloned().unwrap();

                if let Ok(remote_params_candidate) = remote_params_candidate(reveal, remote_commit)
                {
                    debug!("{:?} sets remote_params", self.state.swap_role());
                    self.state.sup_remote_params(remote_params_candidate);
                } else {
                    error!("Revealed remote params not preimage of commitment");
                }

                // Specific to swap roles
                // pass request on to wallet daemon so that it can set remote params
                match self.state.swap_role() {
                    // validated state above, no need to check again
                    SwapRole::Alice => {
                        // Alice already sends RevealProof immediately, so only have to
                        // forward Reveal now
                        trace!(
                            "sending request {} to {} on bus {}",
                            &request,
                            &ServiceId::Wallet,
                            &ServiceBus::Msg
                        );
                        self.send_wallet(msg_bus, endpoints, request)?
                    }
                    SwapRole::Bob
                        if self.syncer_state.btc_fee_estimate_sat_per_kvb.is_some()
                            && self.state.b_address().is_some() =>
                    {
                        let address = self.state.b_address().cloned().unwrap();
                        let sat_per_kvb = self.syncer_state.btc_fee_estimate_sat_per_kvb.unwrap();
                        self.ask_bob_to_fund(sat_per_kvb, address, endpoints)?;

                        // sending this request will initialize the
                        // arbitrating setup, that can be only performed
                        // after the funding tx was seen
                        let pending_req = PendingRequest::new(
                            self.identity(),
                            ServiceId::Wallet,
                            msg_bus,
                            request,
                        );
                        self.pending_requests
                            .defer_request(ServiceId::Wallet, pending_req);
                    }
                    _ => unreachable!(
                        "Bob btc_fee_estimate_sat_per_kvb.is_none() was handled previously"
                    ),
                }

                // up to here for both maker and taker, following only Maker

                // if did not yet reveal, maker only. on the msg flow as
                // of 2021-07-13 taker reveals first
                if self.state.commit() && self.state.trade_role() == Some(TradeRole::Maker) {
                    if let Some(addr) = self.state.b_address().cloned() {
                        let txlabel = TxLabel::Funding;
                        if !self.syncer_state.is_watched_addr(&txlabel) {
                            let watch_addr_task = self.syncer_state.watch_addr_btc(addr, txlabel);
                            endpoints.send_to(
                                ServiceBus::Sync,
                                self.identity(),
                                self.syncer_state.bitcoin_syncer(),
                                BusMsg::Sync(SyncMsg::Task(watch_addr_task)),
                            )?;
                        }
                    }
                }
            }
            // alice receives, bob sends
            Msg::CoreArbitratingSetup(CoreArbitratingSetup {
                lock,
                cancel,
                refund,
                ..
            }) if self.state.swap_role() == SwapRole::Alice && self.state.reveal() => {
                for (&tx, tx_label) in [lock, cancel, refund].iter().zip([
                    TxLabel::Lock,
                    TxLabel::Cancel,
                    TxLabel::Refund,
                ]) {
                    let tx = tx.clone().extract_tx();
                    let txid = tx.txid();
                    debug!(
                        "tx_label: {}, vsize: {}, outs: {}",
                        tx_label,
                        tx.vsize(),
                        tx.output.len()
                    );
                    if !self.syncer_state.is_watched_tx(&tx_label) {
                        let task = self.syncer_state.watch_tx_btc(txid, tx_label);
                        endpoints.send_to(
                            ServiceBus::Sync,
                            self.identity(),
                            self.syncer_state.bitcoin_syncer(),
                            BusMsg::Sync(SyncMsg::Task(task)),
                        )?;
                    }
                    if tx_label == TxLabel::Refund {
                        self.syncer_state.tasks.txids.insert(TxLabel::Refund, txid);
                    }
                }
                self.send_wallet(msg_bus, endpoints, request)?;
            }
            // bob receives, alice sends
            Msg::RefundProcedureSignatures(_) if self.state.b_core_arb() => {
                self.state.sup_received_refund_procedure_signatures();
                self.send_wallet(msg_bus, endpoints, request)?;
            }
            // alice receives, bob sends
            Msg::BuyProcedureSignature(buy_proc_sig)
                if self.state.a_refundsig() && !self.state.a_overfunded() =>
            {
                // Alice verifies that she has sent refund procedure signatures before
                // processing the buy signatures from Bob
                let tx_label = TxLabel::Buy;
                if !self.syncer_state.is_watched_tx(&tx_label) {
                    let txid = buy_proc_sig.buy.clone().extract_tx().txid();
                    let task = self.syncer_state.watch_tx_btc(txid, tx_label);
                    endpoints.send_to(
                        ServiceBus::Sync,
                        self.identity(),
                        self.syncer_state.bitcoin_syncer(),
                        BusMsg::Sync(SyncMsg::Task(task)),
                    )?;
                }

                // checkpoint swap alice pre buy
                debug!("{} | checkpointing alice pre buy swapd state", self.swap_id);
                if self.state.a_sup_checkpoint_pre_buy() {
                    checkpoint_send(
                        endpoints,
                        self.swap_id,
                        self.identity(),
                        ServiceId::Database,
                        CheckpointState::CheckpointSwapd(CheckpointSwapd {
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

                self.send_wallet(msg_bus, endpoints, request)?
            }

            // bob and alice
            Msg::Abort(_) => return Err(Error::Farcaster("Abort not yet supported".to_string())),
            Msg::Ping(_) | Msg::Pong(_) | Msg::PingPeer => {
                unreachable!("ping/pong must remain in peerd, and unreachable in swapd")
            }
            request => error!(
                "request {} not supported at msg bus at state {}",
                request, self.state
            ),
        }
        Ok(())
    }

    fn handle_ctl(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: BusMsg,
    ) -> Result<(), Error> {
        match (&request, &source) {
            (BusMsg::Ctl(Ctl::Hello), _) => {
                info!(
                    "{} | Service {} daemon is now {}",
                    self.swap_id.swap_id(),
                    source.bright_green_bold(),
                    "connected"
                );
            }
            (_, ServiceId::Syncer(..)) if self.syncer_state.any_syncer(&source) => {
            }
            (
                _,
                ServiceId::Farcasterd
                | ServiceId::Wallet
                | ServiceId::Database
            ) => {}
            (BusMsg::Ctl(Ctl::AbortSwap), ServiceId::Client(_)) => {}
            _ => return Err(Error::Farcaster(
                "Permission Error: only Farcasterd, Wallet, Client and Syncer can can control swapd"
                    .to_string(),
            )),
        };

        match request {
            BusMsg::Ctl(Ctl::Terminate) if source == ServiceId::Farcasterd => {
                info!(
                    "{} | {}",
                    self.swap_id.swap_id(),
                    format!("Terminating {}", self.identity()).label()
                );
                std::process::exit(0);
            }

            BusMsg::Ctl(Ctl::TakeSwap(InitSwap {
                peerd,
                report_to,
                local_params,
                swap_id,
                remote_commit: None,
                funding_address, // Some(_) for Bob, None for Alice
            })) if self.state.start() => {
                if ServiceId::Swap(swap_id) != self.identity {
                    error!(
                        "{}: {}",
                        "This swapd instance is not reponsible for swap_id", swap_id
                    );
                    return Ok(());
                };
                self.syncer_state.watch_fee_and_height(endpoints)?;

                self.peer_service = peerd.clone();
                self.enquirer = report_to.clone();

                if let ServiceId::Peer(ref addr) = peerd {
                    self.maker_peer = Some(addr.clone());
                }
                let local_commit =
                    self.taker_commit(endpoints, local_params.clone())
                        .map_err(|err| {
                            error!("{}", err);
                            self.report_failure_to(
                                endpoints,
                                &report_to,
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
                let take_swap = TakeCommit {
                    commit: local_commit,
                    public_offer: self.public_offer.clone(),
                    swap_id,
                };
                self.send_peer(endpoints, Msg::TakerCommit(take_swap))?;
                self.state_update(endpoints, next_state)?;
            }

            BusMsg::Msg(Msg::Reveal(Reveal::Proof(proof)))
                if self.state.commit() && self.state.remote_commit().is_some() =>
            {
                let reveal_proof = Msg::Reveal(Reveal::Proof(proof));
                let swap_id = reveal_proof.swap_id();
                self.send_peer(endpoints, reveal_proof)?;
                trace!("sent reveal_proof to peerd");
                let local_params = self
                    .state
                    .local_params()
                    .expect("commit state has local_params");
                let reveal_params = match (swap_id, local_params.clone()) {
                    (swap_id, Params::Alice(params)) => {
                        Reveal::AliceParameters(params.reveal_alice(swap_id))
                    }
                    (swap_id, Params::Bob(params)) => {
                        Reveal::BobParameters(params.reveal_bob(swap_id))
                    }
                };
                self.send_peer(endpoints, Msg::Reveal(reveal_params))?;
                trace!("sent reveal_proof to peerd");
                let next_state = self.state.clone().sup_commit_to_reveal();
                self.state_update(endpoints, next_state)?;
            }

            BusMsg::Ctl(Ctl::MakeSwap(InitSwap {
                peerd,
                report_to,
                local_params,
                swap_id,
                remote_commit: Some(remote_commit),
                funding_address, // Some(_) for Bob, None for Alice
            })) if self.state.start() => {
                self.syncer_state.watch_fee_and_height(endpoints)?;
                self.peer_service = peerd.clone();
                if let ServiceId::Peer(ref addr) = peerd {
                    self.maker_peer = Some(addr.clone());
                }
                self.enquirer = report_to.clone();
                let local_commit = self
                    .maker_commit(endpoints, &peerd, swap_id, &local_params)
                    .map_err(|err| {
                        self.report_failure_to(
                            endpoints,
                            &report_to,
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
                    Some(remote_commit),
                );

                trace!("sending peer MakerCommit msg {}", &local_commit);
                self.send_peer(endpoints, Msg::MakerCommit(local_commit))?;
                self.state_update(endpoints, next_state)?;
            }

            BusMsg::Ctl(Ctl::FundingUpdated)
                if source == ServiceId::Wallet
                    && ((self.state.trade_role() == Some(TradeRole::Taker)
                        && self.state.reveal())
                        || (self.state.trade_role() == Some(TradeRole::Maker)
                            && self.state.commit()))
                    && self.pending_requests().contains_key(&source)
                    && self
                        .pending_requests()
                        .get(&source)
                        .map(|reqs| reqs.len() == 2)
                        .unwrap() =>
            {
                let success_proof = PendingRequests::continue_deferred_requests(
                    self,
                    endpoints,
                    source.clone(),
                    |r| {
                        matches!(
                            r,
                            &PendingRequest {
                                dest: ServiceId::Wallet,
                                bus_id: ServiceBus::Msg,
                                request: BusMsg::Msg(Msg::Reveal(Reveal::Proof(_))),
                                ..
                            }
                        )
                    },
                );
                if !success_proof {
                    error!("Did not dispatch proof pending request");
                }

                let success_params =
                    PendingRequests::continue_deferred_requests(self, endpoints, source, |r| {
                        matches!(
                            r,
                            &PendingRequest {
                                dest: ServiceId::Wallet,
                                bus_id: ServiceBus::Msg,
                                request: BusMsg::Msg(Msg::Reveal(Reveal::AliceParameters(_))),
                                ..
                            }
                        )
                    });
                if !success_params {
                    error!("Did not dispatch params pending requests");
                }
            }

            BusMsg::Msg(Msg::CoreArbitratingSetup(core_arb_setup))
                if self.state.reveal()
                    && self.state.remote_params().is_some()
                    && self.state.local_params().is_some() =>
            {
                // checkpoint swap pre lock bob
                debug!("{} | checkpointing bob pre lock swapd state", self.swap_id);
                if self.state.b_sup_checkpoint_pre_lock() {
                    checkpoint_send(
                        endpoints,
                        self.swap_id,
                        self.identity(),
                        ServiceId::Database,
                        CheckpointState::CheckpointSwapd(CheckpointSwapd {
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
                for (tx, tx_label) in [lock, cancel, refund].iter().zip([
                    TxLabel::Lock,
                    TxLabel::Cancel,
                    TxLabel::Refund,
                ]) {
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
                trace!("sending peer CoreArbitratingSetup msg: {}", &core_arb_setup);
                self.send_peer(endpoints, Msg::CoreArbitratingSetup(core_arb_setup))?;
                let next_state = State::Bob(BobState::CorearbB {
                    received_refund_procedure_signatures: false,
                    local_params: self.state.local_params().cloned().unwrap(),
                    cancel_seen: false,
                    remote_params: self.state.remote_params().unwrap(),
                    b_address: self.state.b_address().cloned().unwrap(),
                    last_checkpoint_type: self.state.last_checkpoint_type().unwrap(),
                    buy_tx_seen: false,
                });
                self.state_update(endpoints, next_state)?;
            }

            BusMsg::Ctl(Ctl::Tx(Tx::Lock(btc_lock))) if self.state.b_core_arb() => {
                log_tx_received(self.swap_id, TxLabel::Lock);
                self.broadcast(btc_lock, TxLabel::Lock, endpoints)?;
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
            }
            BusMsg::Ctl(Ctl::Tx(transaction)) => {
                // update state
                match transaction.clone() {
                    Tx::Cancel(tx) => {
                        log_tx_received(self.swap_id, TxLabel::Cancel);
                        self.txs.insert(TxLabel::Cancel, tx);
                    }
                    Tx::Refund(tx) => {
                        log_tx_received(self.swap_id, TxLabel::Refund);
                        self.txs.insert(TxLabel::Refund, tx);
                    }
                    Tx::Punish(tx) => {
                        log_tx_received(self.swap_id, TxLabel::Punish);
                        self.txs.insert(TxLabel::Punish, tx);
                    }
                    Tx::Buy(tx) => {
                        log_tx_received(self.swap_id, TxLabel::Buy);
                        self.txs.insert(TxLabel::Buy, tx);
                    }
                    Tx::Funding(_) => unreachable!("not handled in swapd"),
                    Tx::Lock(_) => unreachable!("handled above"),
                }
                // replay last tx confirmation event received from syncer, recursing
                let source = self.syncer_state.bitcoin_syncer();
                match transaction {
                    Tx::Cancel(_) | Tx::Buy(_) => {
                        if let Some(lock_tx_confs_req) = self.syncer_state.lock_tx_confs.clone() {
                            self.handle_sync(endpoints, source, lock_tx_confs_req)?;
                        }
                    }
                    Tx::Refund(_) | Tx::Punish(_) => {
                        if let Some(cancel_tx_confs_req) = self.syncer_state.cancel_tx_confs.clone()
                        {
                            self.handle_sync(endpoints, source, cancel_tx_confs_req)?;
                        }
                    }
                    _ => {}
                }
            }

            BusMsg::Ctl(Ctl::SweepAddress(sweep_address)) => match sweep_address {
                SweepAddressAddendum::Bitcoin(sweep_btc) => {
                    info!(
                        "{} | Sweeping source (funding) address: {} to destination address: {}",
                        self.swap_id.swap_id(),
                        sweep_btc.source_address.addr(),
                        sweep_btc.destination_address.addr()
                    );
                    let task = self.syncer_state.sweep_btc(sweep_btc.clone(), false);
                    endpoints.send_to(
                        ServiceBus::Sync,
                        self.identity(),
                        self.syncer_state.bitcoin_syncer(),
                        BusMsg::Sync(SyncMsg::Task(task)),
                    )?;
                }
                SweepAddressAddendum::Monero(sweep_xmr) => {
                    let task = self.syncer_state.sweep_xmr(sweep_xmr.clone(), true);
                    let acc_confs_needs = self.temporal_safety.sweep_monero_thr
                        - self.temporal_safety.xmr_finality_thr;
                    let sweep_block =
                        self.syncer_state.height(Blockchain::Monero) + acc_confs_needs as u64;
                    info!(
                        "{} | Tx {} needs {}, and has {} {}",
                        self.swap_id.swap_id(),
                        TxLabel::AccLock.label(),
                        "10 confirmations".bright_green_bold(),
                        (10 - acc_confs_needs).bright_green_bold(),
                        "confirmations".bright_green_bold(),
                    );
                    info!(
                        "{} | {} reaches your address {} around block {}",
                        self.swap_id.swap_id(),
                        Blockchain::Monero.label(),
                        sweep_xmr.destination_address.addr(),
                        sweep_block.bright_blue_bold(),
                    );
                    warn!(
                        "Peerd might crash, just ignore it, counterparty closed\
                               connection but you don't need it anymore!"
                    );
                    let request = BusMsg::Sync(SyncMsg::Task(task));
                    let dest = self.syncer_state.monero_syncer();
                    let pending_request = PendingRequest::new(
                        self.identity(),
                        dest.clone(),
                        ServiceBus::Sync,
                        request,
                    );
                    self.pending_requests.defer_request(dest, pending_request);
                }
            },

            BusMsg::Msg(Msg::RefundProcedureSignatures(refund_proc_sigs))
                if self.state.reveal()
                    && self.state.remote_params().is_some()
                    && self.state.local_params().is_some() =>
            {
                // checkpoint alice pre lock bob
                debug!(
                    "{} | checkpointing alice pre lock swapd state",
                    self.swap_id
                );
                if self.state.a_sup_checkpoint_pre_lock() {
                    checkpoint_send(
                        endpoints,
                        self.swap_id,
                        self.identity(),
                        ServiceId::Database,
                        CheckpointState::CheckpointSwapd(CheckpointSwapd {
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

                self.send_peer(endpoints, Msg::RefundProcedureSignatures(refund_proc_sigs))?;
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
                self.state_update(endpoints, next_state)?;
            }

            BusMsg::Msg(Msg::BuyProcedureSignature(ref buy_proc_sig))
                if self.state.b_core_arb()
                    && !self.syncer_state.tasks.txids.contains_key(&TxLabel::Buy) =>
            {
                // checkpoint bob pre buy
                debug!("{} | checkpointing bob pre buy swapd state", self.swap_id);
                if self.state.b_sup_checkpoint_pre_buy() {
                    checkpoint_send(
                        endpoints,
                        self.swap_id,
                        self.identity(),
                        ServiceId::Database,
                        CheckpointState::CheckpointSwapd(CheckpointSwapd {
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
                    endpoints.send_to(
                        ServiceBus::Sync,
                        self.identity(),
                        self.syncer_state.bitcoin_syncer(),
                        BusMsg::Sync(SyncMsg::Task(task)),
                    )?;
                }
                // set external eddress: needed to subscribe for buy tx (bob) or refund (alice)
                self.syncer_state.tasks.txids.insert(TxLabel::Buy, txid);
                let pending_request = PendingRequest::new(
                    self.identity(),
                    self.peer_service.clone(),
                    ServiceBus::Msg,
                    request,
                );
                self.pending_requests
                    .defer_request(self.syncer_state.monero_syncer(), pending_request);
            }

            BusMsg::Ctl(Ctl::AbortSwap)
                if self.state.a_start()
                    || self.state.a_commit()
                    || self.state.a_reveal()
                    || (self.state.a_refundsig() && !self.state.a_btc_locked()) =>
            {
                // just cancel the swap, no additional logic required
                self.state_update(endpoints, State::Alice(AliceState::FinishA(Outcome::Abort)))?;
                self.abort_swap(endpoints)?;
                if let ServiceId::Client(_) = source {
                    self.send_ctl(
                        endpoints,
                        source,
                        BusMsg::Rpc(Rpc::String("Aborted swap".to_string())),
                    )?;
                }
            }
            BusMsg::Ctl(Ctl::AbortSwap) if self.state.b_start() => {
                // just cancel the swap, no additional logic required, since funding was not yet retrieved
                self.state_update(endpoints, State::Bob(BobState::FinishB(Outcome::Abort)))?;
                self.abort_swap(endpoints)?;
                if let ServiceId::Client(_) = source {
                    self.send_ctl(
                        endpoints,
                        source,
                        BusMsg::Rpc(Rpc::String("Aborted swap".to_string())),
                    )?;
                }
            }
            BusMsg::Ctl(Ctl::AbortSwap)
                if self.state.b_commit()
                    || self.state.b_reveal()
                    || (!self.state.b_received_refund_procedure_signatures()
                        && self.state.b_core_arb()) =>
            {
                self.send_ctl(
                    endpoints,
                    ServiceId::Wallet,
                    BusMsg::Ctl(Ctl::GetSweepBitcoinAddress(
                        self.state.b_address().cloned().unwrap(),
                    )),
                )?;
                // cancel the swap to invalidate its state
                self.state_update(endpoints, State::Bob(BobState::FinishB(Outcome::Abort)))?;
                if let ServiceId::Client(_) = source {
                    self.send_ctl(
                        endpoints,
                        source,
                        BusMsg::Rpc(Rpc::String(
                            "Aborting swap, checking if funds can be sweeped.".to_string(),
                        )),
                    )?;
                }
            }
            BusMsg::Ctl(Ctl::AbortSwap) => {
                let msg = "Swap is already locked-in, cannot manually abort anymore.".to_string();
                warn!("{} | {}", self.swap_id.swap_id(), msg);

                if let ServiceId::Client(_) = source {
                    self.send_ctl(
                        endpoints,
                        source,
                        BusMsg::Rpc(Rpc::Failure(Failure {
                            code: FailureCode::Unknown,
                            info: msg,
                        })),
                    )?;
                }
            }
            BusMsg::Ctl(Ctl::PeerdReconnected(service_id)) => {
                // set the reconnected service id, if it is not set yet. This
                // can happen if this is a maker launched swap after restoration
                // and the taker reconnects
                if self.peer_service == ServiceId::Loopback {
                    self.peer_service = service_id;
                }
                for msg in self.pending_peer_request.clone().iter() {
                    self.send_peer(endpoints, msg.clone())?;
                }
                self.pending_peer_request.clear();
            }

            BusMsg::Ctl(Ctl::Checkpoint(Checkpoint { swap_id, state })) => match state {
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
                    info!("{} | Restoring swap", swap_id.swap_id());
                    self.state = state;
                    self.enquirer = enquirer;
                    self.temporal_safety = temporal_safety;
                    self.pending_requests = pending_requests;
                    self.txs = txs.clone();
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
                    let msg = format!("Restored swap at state {}", self.state);
                    let _ = self.report_progress_message_to(endpoints, ServiceId::Farcasterd, msg);

                    self.handle_ctl(endpoints, ServiceId::Database, BusMsg::Msg(last_msg))?;
                }
                s => {
                    error!("Checkpoint {} not supported in swapd", s);
                }
            },

            _ => {
                error!("BusMsg is not supported by the CTL interface {}", request);
                return Err(Error::NotSupported(ServiceBus::Ctl, request.to_string()));
            }
        }

        Ok(())
    }

    fn handle_rpc(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: Rpc,
    ) -> Result<(), Error> {
        match request {
            Rpc::GetInfo => {
                let swap_id = if self.swap_id() == zero!() {
                    None
                } else {
                    Some(self.swap_id())
                };
                let info = SwapInfo {
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
                self.send_client_rpc(endpoints, source, Rpc::SwapInfo(info))?;
            }

            req => {
                warn!("Ignoring request: {}", req.err());
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
                                    BusMsg::Ctl(Ctl::FundingCompleted(Blockchain::Monero)),
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
                            BusMsg::Sync(SyncMsg::Task(Task::SweepAddress(mut task))),
                            ServiceBus::Sync,
                        ) = (request.clone(), bus_id)
                        {
                            // safe cast
                            task.from_height =
                                Some(self.syncer_state.monero_height - *confirmations as u64);
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
                                        request: BusMsg::Msg(Msg::BuyProcedureSignature(_)),
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
                            self.state_update(endpoints, next_state)?;
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
                                        BusMsg::Ctl(Ctl::FundingCompleted(Blockchain::Monero)),
                                    )?;
                                }
                                SwapRole::Bob => {
                                    endpoints.send_to(
                                        ServiceBus::Ctl,
                                        self.identity(),
                                        ServiceId::Farcasterd,
                                        BusMsg::Ctl(Ctl::FundingCompleted(Blockchain::Bitcoin)),
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
                            self.state_update(
                                endpoints,
                                State::Bob(BobState::FinishB(Outcome::Buy)),
                            )?;
                            Some(Outcome::Buy)
                        } else if self.state.a_refund_seen() {
                            self.state_update(
                                endpoints,
                                State::Alice(AliceState::FinishA(Outcome::Refund)),
                            )?;
                            Some(Outcome::Refund)
                        } else {
                            error!("Unexpected sweeping state, not sending finalization commands to wallet and farcasterd");
                            None
                        };
                        if let Some(success) = success {
                            let swap_success_req = BusMsg::Ctl(Ctl::SwapOutcome(success));
                            self.send_ctl(endpoints, ServiceId::Wallet, swap_success_req.clone())?;
                            self.send_ctl(endpoints, ServiceId::Farcasterd, swap_success_req)?;
                            // remove txs to invalidate outdated states
                            self.txs.remove(&TxLabel::Cancel);
                            self.txs.remove(&TxLabel::Refund);
                            self.txs.remove(&TxLabel::Buy);
                            self.txs.remove(&TxLabel::Punish);
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
                        let tx = bitcoin::Transaction::deserialize(tx)?;
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
                                    self.handle_ctl(
                                        endpoints,
                                        source,
                                        BusMsg::Ctl(Ctl::AbortSwap),
                                    )?;
                                    return Ok(());
                                } else {
                                    // funding completed, amount is correct
                                    endpoints.send_to(
                                        ServiceBus::Ctl,
                                        self.identity(),
                                        ServiceId::Farcasterd,
                                        BusMsg::Ctl(Ctl::FundingCompleted(Blockchain::Bitcoin)),
                                    )?;
                                }

                                // forward tx to wallet
                                let req = BusMsg::Ctl(Ctl::Tx(Tx::Funding(tx)));
                                self.send_wallet(ServiceBus::Ctl, endpoints, req)?;
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
                        let tx = bitcoin::Transaction::deserialize(tx)?;
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
                                let req = BusMsg::Ctl(Ctl::Tx(Tx::Buy(tx.clone())));
                                self.send_wallet(ServiceBus::Ctl, endpoints, req)?
                            }
                            TxLabel::Buy if self.state.b_core_arb() => {
                                log_tx_seen(self.swap_id, &txlabel, &tx.txid());
                                self.state.b_sup_buy_tx_seen();
                                let req = BusMsg::Ctl(Ctl::Tx(Tx::Buy(tx.clone())));
                                self.send_wallet(ServiceBus::Ctl, endpoints, req)?;

                                // The buy transaction is received in Corearb state, go straight to buy state
                                let next_state = State::Bob(BobState::BuySigB {
                                    buy_tx_seen: false,
                                    last_checkpoint_type: self.state.last_checkpoint_type().unwrap(),
                                });
                                self.state_update(endpoints, next_state)?;
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
                                if self.state.a_refundsig()
                                    && self.state.a_xmr_locked()
                                // && !self.state.a_buy_published()
                                =>
                            {
                                log_tx_seen(self.swap_id, &txlabel, &tx.txid());
                                let req = BusMsg::Ctl(Ctl::Tx(Tx::Refund(tx.clone())));
                                self.send_wallet(ServiceBus::Ctl, endpoints, req)?
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
                                    let viewpair = monero::ViewPair { spend, view };
                                    let address = monero::Address::from_viewpair(
                                        self.syncer_state.network.into(),
                                        &viewpair,
                                    );
                                    let swap_id = self.swap_id();
                                    let amount = self.syncer_state.monero_amount;
                                    info!(
                                        "{} | Send {} to {}",
                                        swap_id.swap_id(),
                                        amount.bright_green_bold(),
                                        address.addr(),
                                    );
                                    self.state.a_sup_required_funding_amount(amount);
                                    let funding_request = BusMsg::Ctl(Ctl::FundingInfo(
                                        FundingInfo::Monero(MoneroFundingInfo {
                                            swap_id,
                                            address,
                                            amount,
                                        }),
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
                                    let txlabel = TxLabel::AccLock;
                                    if !self.syncer_state.is_watched_addr(&txlabel) {
                                        let watch_addr_task = self
                                            .syncer_state
                                            .watch_addr_xmr(spend, view, txlabel, None);
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
                                    BusMsg::Ctl(Ctl::FundingCanceled(Blockchain::Monero)),
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
                                if self.temporal_safety.safe_refund(*confirmations)
                                    && (self.state.b_buy_sig() || self.state.b_core_arb())
                                    && self.txs.contains_key(&TxLabel::Refund) =>
                            {
                                trace!("here Bob publishes refund tx");
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
                                        BusMsg::Ctl(Ctl::FundingCanceled(Blockchain::Monero)),
                                    )?;
                                    self.syncer_state.awaiting_funding = false;
                                }
                                self.state_update(
                                    endpoints,
                                    State::Alice(AliceState::FinishA(Outcome::Refund)),
                                )?;
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
                                    BusMsg::Ctl(Ctl::SwapOutcome(Outcome::Refund));
                                self.send_wallet(
                                    ServiceBus::Ctl,
                                    endpoints,
                                    swap_success_req.clone(),
                                )?;
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
                                // FIXME: swap ends here for alice
                                // wallet + farcaster
                                self.state_update(
                                    endpoints,
                                    State::Alice(AliceState::FinishA(Outcome::Buy)),
                                )?;
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
                                let swap_success_req = BusMsg::Ctl(Ctl::SwapOutcome(Outcome::Buy));
                                self.send_wallet(
                                    ServiceBus::Ctl,
                                    endpoints,
                                    swap_success_req.clone(),
                                )?;
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
                                self.state_update(
                                    endpoints,
                                    State::Bob(BobState::FinishB(Outcome::Refund)),
                                )?;
                                let swap_success_req =
                                    BusMsg::Ctl(Ctl::SwapOutcome(Outcome::Refund));
                                self.send_ctl(
                                    endpoints,
                                    ServiceId::Wallet,
                                    swap_success_req.clone(),
                                )?;
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
                                    SwapRole::Alice => self.state_update(
                                        endpoints,
                                        State::Alice(AliceState::FinishA(Outcome::Punish)),
                                    )?,
                                    SwapRole::Bob => {
                                        warn!("{}", "You were punished!".err());
                                        self.state_update(
                                            endpoints,
                                            State::Bob(BobState::FinishB(Outcome::Punish)),
                                        )?
                                    }
                                }
                                let swap_success_req =
                                    BusMsg::Ctl(Ctl::SwapOutcome(Outcome::Punish));
                                self.send_ctl(
                                    endpoints,
                                    ServiceId::Wallet,
                                    swap_success_req.clone(),
                                )?;
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
                        self.state_update(
                            endpoints,
                            State::Bob(BobState::FinishB(Outcome::Abort)),
                        )?;
                        endpoints.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            ServiceId::Farcasterd,
                            BusMsg::Ctl(Ctl::FundingCanceled(Blockchain::Bitcoin)),
                        )?;
                        self.abort_swap(endpoints)?;
                    }

                    Event::SweepSuccess(event) => {
                        debug!("{}", event)
                    }

                    Event::TransactionRetrieved(event) => {
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
                                            request: BusMsg::Msg(Msg::Reveal(
                                                Reveal::AliceParameters(..)
                                            )),
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
                }
            }

            _ => {
                warn!("BusMsg is not handled by the SYNC interface {}", request);
            }
        }

        Ok(())
    }
}

impl Runtime {
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
        let req = BusMsg::Ctl(Ctl::FundingInfo(FundingInfo::Bitcoin(BitcoinFundingInfo {
            swap_id,
            address,
            amount,
        })));
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
        let enquirer = self.enquirer.clone();
        // Ignoring possible reporting errors here and after: do not want to
        // halt the swap just because the client disconnected
        let _ = self.report_progress_message_to(endpoints, &enquirer, msg);

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
        peerd: &ServiceId,
        swap_id: SwapId,
        params: &Params,
    ) -> Result<Commit, Error> {
        info!(
            "{} | {} as Maker from Taker through peerd {}",
            swap_id.swap_id(),
            "Accepting swap".bright_white_bold(),
            peerd.bright_blue_italic()
        );

        let msg = format!(
            "Accepting swap {} as Maker from Taker through peerd {}",
            swap_id, peerd
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
        let swap_success_req = BusMsg::Ctl(Ctl::SwapOutcome(Outcome::Abort));
        self.send_ctl(endpoints, ServiceId::Wallet, swap_success_req.clone())?;
        self.send_ctl(endpoints, ServiceId::Farcasterd, swap_success_req)?;
        info!("{} | Aborted swap.", self.swap_id.swap_id());
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

fn remote_params_candidate(reveal: &Reveal, remote_commit: Commit) -> Result<Params, Error> {
    // parameter processing irrespective of maker & taker role
    let core_wallet = CommitmentEngine;
    match reveal {
        Reveal::AliceParameters(reveal) => match &remote_commit {
            Commit::AliceParameters(commit) => {
                commit.verify_with_reveal(&core_wallet, reveal.clone())?;
                Ok(Params::Alice(reveal.clone().into_parameters()))
            }
            _ => {
                let err_msg = "expected Some(Commit::Alice(commit))";
                error!("{}", err_msg);
                Err(Error::Farcaster(err_msg.to_string()))
            }
        },
        Reveal::BobParameters(reveal) => match &remote_commit {
            Commit::BobParameters(commit) => {
                commit.verify_with_reveal(&core_wallet, reveal.clone())?;
                Ok(Params::Bob(reveal.clone().into_parameters()))
            }
            _ => {
                let err_msg = "expected Some(Commit::Bob(commit))";
                error!("{}", err_msg);
                Err(Error::Farcaster(err_msg.to_string()))
            }
        },
        Reveal::Proof(_) => Err(Error::Farcaster(s!(
            "this should have been caught by another pattern!"
        ))),
    }
}
