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

use crate::databased::checkpoint_send;
use crate::service::Endpoints;
use crate::syncerd::bitcoin_syncer::p2wpkh_signed_tx_fee;
use crate::syncerd::{FeeEstimation, FeeEstimations};
use crate::{
    rpc::request::Outcome,
    rpc::request::{BitcoinFundingInfo, FundingInfo, MoneroFundingInfo},
    syncerd::{
        Abort, GetTx, HeightChanged, SweepAddress, SweepAddressAddendum, SweepMoneroAddress,
        SweepSuccess, TaskId, TaskTarget, TransactionRetrieved, WatchHeight, XmrAddressAddendum,
    },
};
use std::{
    any::Any,
    collections::{BTreeMap, HashMap, HashSet},
    convert::TryInto,
};
use std::{convert::TryFrom, str::FromStr};
use std::{
    io::Cursor,
    time::{Duration, SystemTime},
};

use super::{
    storage::{self, Driver},
    swap_state::{AliceState, BobState, State, SwapCheckpointType},
    syncer_client::{log_tx_received, log_tx_seen, SyncerState, SyncerTasks},
    temporal_safety::TemporalSafety,
};
use crate::rpc::{
    request::{self, Failure, FailureCode, Msg},
    Request, ServiceBus,
};
use crate::{CtlServer, Error, LogStyle, Service, ServiceConfig, ServiceId};
use bitcoin::{consensus::Encodable, secp256k1};
use bitcoin::{
    hashes::{hex::FromHex, ripemd160, sha256, Hash, HashEngine},
    Txid,
};
use bitcoin::{
    util::psbt::{serialize::Deserialize, PartiallySignedTransaction},
    Script,
};

use crate::syncerd::types::{
    AddressAddendum, AddressTransaction, Boolean, BroadcastTransaction, BtcAddressAddendum, Event,
    Task, TransactionConfirmations, WatchAddress, WatchTransaction,
};
use farcaster_core::{
    bitcoin::{
        fee::SatPerVByte, segwitv0::LockTx, segwitv0::SegwitV0, timelock::CSVTimelock, Bitcoin,
        BitcoinSegwitV0,
    },
    blockchain::{self, Blockchain, FeeStrategy},
    consensus::{self, Encodable as FarEncodable},
    crypto::{CommitmentEngine, SharedKeyId, TaggedElement},
    monero::{Monero, SHARED_VIEW_KEY_ID},
    role::{SwapRole, TradeRole},
    swap::btcxmr::{
        message::{CommitAliceParameters, CommitBobParameters, CoreArbitratingSetup},
        Offer, Parameters, PublicOffer,
    },
    swap::SwapId,
    transaction::{Broadcastable, Transaction, TxLabel, Witnessable},
};
use internet2::zeromq::{self, ZmqSocketType};
use internet2::{
    addr::NodeAddr, session, CreateUnmarshaller, Session, TypedEnum, Unmarshall, Unmarshaller,
};
use microservices::esb::{self, Handler};
use monero::{cryptonote::hash::keccak_256, PrivateKey, ViewPair};
use request::{Checkpoint, CheckpointState, Commit, InitSwap, Params, Reveal, TakeCommit, Tx};
use std::net::SocketAddr;
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
        format!("{:#x}", swap_id).addr()
    );
    info!(
        "{} | Initial state: {}",
        swap_id.bright_blue_italic(),
        init_state.bright_white_bold()
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
        storage: Box::new(storage::DiskDriver::init(
            swap_id,
            Box::new(storage::DiskConfig {
                path: Default::default(),
            }),
        )?),
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
    pub peer_service: ServiceId,
    pub state: State,
    pub maker_peer: Option<NodeAddr>,
    pub started: SystemTime,
    pub enquirer: Option<ServiceId>,
    pub syncer_state: SyncerState,
    pub temporal_safety: TemporalSafety,
    pub pending_requests: PendingRequests,
    pub pending_peer_request: Vec<request::Msg>, // Peer requests that failed and are waiting for reconnection
    pub txs: HashMap<TxLabel, bitcoin::Transaction>,
    #[allow(dead_code)]
    storage: Box<dyn storage::Driver>,
    pub public_offer: PublicOffer,
}

// FIXME Something more meaningful than ServiceId to index
pub type PendingRequests = HashMap<ServiceId, Vec<PendingRequest>>;

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
                            if let Ok(_) = match &r.bus_id {
                                ServiceBus::Ctl if &r.dest == &runtime.identity => runtime
                                    .handle_ctl(endpoints, r.source.clone(), r.request.clone()),
                                ServiceBus::Msg if &r.dest == &runtime.identity => runtime
                                    .handle_p2p(endpoints, r.source.clone(), r.request.clone()),
                                _ => endpoints
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
            false
        };
        if !success {
            error!("no deferred request consumed with this predicate");
        }
        success
    }
}

pub trait PendingRequestsT {
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
    pub source: ServiceId,
    pub dest: ServiceId,
    pub bus_id: ServiceBus,
    pub request: Request,
}

impl PendingRequest {
    pub fn new(source: ServiceId, dest: ServiceId, bus_id: ServiceBus, request: Request) -> Self {
        PendingRequest {
            source,
            dest,
            bus_id,
            request,
        }
    }
    pub fn from_event(event: &crate::automata::Event<Request>, bus: ServiceBus) -> Self {
        Self::new(
            event.source.clone(),
            event.service.clone(),
            bus,
            event.message.clone(),
        )
    }
    pub fn from_event_forward(
        event: &crate::automata::Event<Request>,
        bus: ServiceBus,
        dest: ServiceId,
    ) -> Self {
        Self::new(event.service.clone(), dest, bus, event.message.clone())
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
        let unmarshaller: Unmarshaller<Request> = Request::create_unmarshaller();
        let source = ServiceId::strict_decode(&mut d)?;
        let dest = ServiceId::strict_decode(&mut d)?;
        let bus_id = ServiceBus::strict_decode(&mut d)?;
        let request: Request = (&*unmarshaller
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

impl CtlServer for Runtime {
    fn enquirer(&self) -> Option<ServiceId> {
        self.enquirer.clone()
    }
}

impl esb::Handler<ServiceBus> for Runtime {
    type Request = Request;
    type Error = Error;

    fn identity(&self) -> ServiceId {
        self.identity.clone()
    }

    fn handle(
        &mut self,
        endpoints: &mut Endpoints,
        bus: ServiceBus,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Self::Error> {
        match bus {
            ServiceBus::Msg => self.handle_p2p(endpoints, source, request),
            ServiceBus::Ctl => self.handle_ctl(endpoints, source, request),
            _ => Err(Error::NotSupported(ServiceBus::Bridge, request.get_type())),
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
    pub fn send_peer(&mut self, endpoints: &mut Endpoints, msg: request::Msg) -> Result<(), Error> {
        trace!("sending peer message {}", msg.bright_yellow_bold());
        if let Err(error) = endpoints.send_to(
            ServiceBus::Msg,
            self.identity(),
            self.peer_service.clone(), // = ServiceId::Loopback
            Request::Protocol(msg.clone()),
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
                Request::PeerdUnreachable(self.peer_service.clone()),
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

    pub fn syncer_state(&self) -> &SyncerState {
        &self.syncer_state
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn pending_requests(&mut self) -> &mut HashMap<ServiceId, Vec<PendingRequest>> {
        &mut self.pending_requests
    }

    pub fn continue_deferred_requests(
        &mut self,
        endpoints: &mut Endpoints,
        key: ServiceId,
        predicate: fn(&PendingRequest) -> bool,
    ) -> bool {
        let success = if let Some(pending_reqs) = self.pending_requests().remove(&key) {
            let len0 = pending_reqs.len();
            let remaining_pending_reqs: Vec<_> = pending_reqs
                .into_iter()
                .filter_map(|r| {
                    if predicate(&r) {
                        if let Ok(_) = match &r.bus_id {
                            ServiceBus::Ctl if &r.dest == &self.identity => {
                                self.handle_ctl(endpoints, r.source.clone(), r.request.clone())
                            }
                            ServiceBus::Msg if &r.dest == &self.identity => {
                                self.handle_p2p(endpoints, r.source.clone(), r.request.clone())
                            }
                            _ => endpoints
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
            self.pending_requests().insert(key, remaining_pending_reqs);
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

    pub fn state_update(
        &mut self,
        endpoints: &mut Endpoints,
        next_state: State,
    ) -> Result<(), Error> {
        info!(
            "{} | State transition: {} -> {}",
            self.swap_id.bright_blue_italic(),
            self.state.bright_white_bold(),
            next_state.bright_white_bold(),
        );
        let msg = format!("{} -> {}", self.state, next_state,);
        self.state = next_state;
        self.report_state_transition_progress_message_to(endpoints, self.enquirer.clone(), msg)?;
        Ok(())
    }

    pub fn broadcast(
        &mut self,
        tx: bitcoin::Transaction,
        tx_label: TxLabel,
        endpoints: &mut Endpoints,
    ) -> Result<(), Error> {
        info!(
            "{} | Broadcasting {} tx ({})",
            self.swap_id.bright_blue_italic(),
            tx_label.bright_white_bold(),
            tx.txid().bright_yellow_italic()
        );
        let task = self.syncer_state.broadcast(tx);
        Ok(endpoints.send_to(
            ServiceBus::Ctl,
            self.identity(),
            self.syncer_state.bitcoin_syncer(),
            Request::SyncerTask(task),
        )?)
    }

    fn handle_p2p(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        if self.peer_service != source {
            return Err(Error::Farcaster(format!(
                "{}: expected {}, found {}",
                "Incorrect peer connection", self.peer_service, source
            )));
        }
        match &request {
            Request::Protocol(msg) if white_list_p2p_msgs(&request) => {
                if msg.swap_id() != self.swap_id() {
                    return Err(Error::Farcaster(format!(
                        "{}: expected {}, found {}",
                        "Incorrect swap_id ",
                        self.swap_id(),
                        msg.swap_id(),
                    )));
                }
                self.process(endpoints, source, request)?;
            }
            _ => {
                error!(
                    "MSG RPC can be only used for forwarding specific farcaster protocol messages"
                );
                return Err(Error::NotSupported(ServiceBus::Msg, request.get_type()));
            }
        };

        Ok(())
    }

    pub fn handle_ctl(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        match (&request, &source) {
            (Request::Hello, _) => {
                info!(
                    "{} | Service {} daemon is now {}",
                    self.swap_id.bright_blue_italic(),
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
            // FIXME rm clone after syncer correctly handled through self.process
            (r, ServiceId::Client(_)) if white_list_rpc_msgs(r) => {
                self.process(endpoints, source.clone(), request.clone())?;
            }
            (r, _) if white_list_ctl_msgs(r) => {
                self.process(endpoints, source.clone(), request.clone())?;
            }
            _ => return Err(Error::Farcaster(
                "Permission Error: only Farcasterd, Wallet, Client and Syncer can can control swapd"
                    .to_string(),
            )),
        };
        match request {
            // Request::SyncerEvent(ref event) => match (&event, source) {
            // handle monero events here
            // }
            Request::SyncerEvent(ref event) if source == self.syncer_state.monero_syncer => {
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
                                    Request::FundingCompleted(Blockchain::Monero),
                                )?;
                                self.syncer_state.awaiting_funding = false;
                            }
                            let task = self.syncer_state.watch_tx_xmr(hash.clone(), txlabel);
                            endpoints.send_to(
                                ServiceBus::Ctl,
                                self.identity(),
                                self.syncer_state.monero_syncer(),
                                Request::SyncerTask(task),
                            )?;
                        }
                        if self.syncer_state.tasks.watched_addrs.remove(id).is_some() {
                            let abort_task = self.syncer_state.abort_task(*id);
                            endpoints.send_to(
                                ServiceBus::Ctl,
                                self.identity(),
                                self.syncer_state.monero_syncer(),
                                Request::SyncerTask(abort_task),
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
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    self.syncer_state.monero_syncer(),
                                    Request::SyncerTask(watch_tx),
                                )?;
                            }
                            let abort_task = self.syncer_state.abort_task(*id);
                            endpoints.send_to(
                                ServiceBus::Ctl,
                                self.identity(),
                                self.syncer_state.monero_syncer(),
                                Request::SyncerTask(abort_task),
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
                            Request::SyncerTask(Task::SweepAddress(mut task)),
                            ServiceBus::Ctl,
                        ) = (request.clone(), bus_id)
                        {
                            // safe cast
                            task.from_height =
                                Some(self.syncer_state.monero_height - *confirmations as u64);
                            let request = Request::SyncerTask(Task::SweepAddress(task));

                            info!(
                                "{} | Monero are spendable now (height {}), sweeping ephemeral wallet",
                                self.swap_id.bright_blue_italic(),
                                self.syncer_state.monero_height.bright_white_bold()
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
                                        request: Request::Protocol(Msg::BuyProcedureSignature(_)),
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
                                        Request::FundingCompleted(Blockchain::Monero),
                                    )?;
                                }
                                SwapRole::Bob => {
                                    endpoints.send_to(
                                        ServiceBus::Ctl,
                                        self.identity(),
                                        ServiceId::Farcasterd,
                                        Request::FundingCompleted(Blockchain::Bitcoin),
                                    )?;
                                }
                            }
                        }
                        let abort_all = Task::Abort(Abort {
                            task_target: TaskTarget::AllTasks,
                            respond: Boolean::False,
                        });
                        endpoints.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            self.syncer_state.monero_syncer(),
                            Request::SyncerTask(abort_all.clone()),
                        )?;
                        endpoints.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            self.syncer_state.bitcoin_syncer(),
                            Request::SyncerTask(abort_all),
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
                            let swap_success_req = Request::SwapOutcome(success);
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
            Request::SyncerEvent(ref event) if source == self.syncer_state.bitcoin_syncer => {
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
                            &tx.txid().addr()
                        );
                        let txlabel = self.syncer_state.tasks.watched_addrs.get(id).unwrap();
                        match txlabel {
                            TxLabel::Funding
                                if self.syncer_state.awaiting_funding
                                    && self.state.b_required_funding_amount().is_some() =>
                            {
                                log_tx_seen(self.swap_id, txlabel, &tx.txid());
                                self.syncer_state.awaiting_funding = false;
                                endpoints.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    ServiceId::Farcasterd,
                                    Request::FundingCompleted(Blockchain::Bitcoin),
                                )?;
                                // If the bitcoin amount does not match the expected funding amount, abort the swap
                                let amount = bitcoin::Amount::from_sat(*amount);
                                let required_funding_amount =
                                    self.state.b_required_funding_amount().unwrap();

                                if amount != required_funding_amount {
                                    let msg = format!("Incorrect amount funded. Required: {}, Funded: {}. Do not fund this swap anymore, will abort and atttempt to sweep the Bitcoin to the provided address.", amount, required_funding_amount);
                                    error!("{}", msg);
                                    self.report_progress_message_to(
                                        endpoints,
                                        ServiceId::Farcasterd,
                                        msg,
                                    )?;
                                    // FIXME: syncer shall not have permission to AbortSwap, replace source by identity?
                                    self.handle_ctl(endpoints, source, Request::AbortSwap)?;
                                    return Ok(());
                                }

                                let req = Request::Tx(Tx::Funding(tx));
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
                                self.state.b_sup_buysig_buy_tx_seen();
                                let req = Request::Tx(Tx::Buy(tx.clone()));
                                self.send_wallet(ServiceBus::Ctl, endpoints, req)?
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
                                let req = Request::Tx(Tx::Refund(tx.clone()));
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
                            ServiceBus::Ctl,
                            self.identity(),
                            self.syncer_state.bitcoin_syncer(),
                            Request::SyncerTask(task.clone()),
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
                                        swap_id.bright_blue_italic(),
                                        amount.bright_green_bold(),
                                        address.addr(),
                                    );
                                    self.state.a_sup_required_funding_amount(amount);
                                    let funding_request = Request::FundingInfo(
                                        FundingInfo::Monero(MoneroFundingInfo {
                                            swap_id,
                                            address,
                                            amount,
                                        }),
                                    );
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
                                            ServiceBus::Ctl,
                                            self.identity(),
                                            self.syncer_state.monero_syncer(),
                                            Request::SyncerTask(watch_addr_task),
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
                                    self.swap_id.bright_blue_italic()
                                );
                                self.syncer_state.awaiting_funding = false;
                                endpoints.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    ServiceId::Farcasterd,
                                    Request::FundingCanceled(Blockchain::Monero),
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
                                        ServiceBus::Ctl,
                                        self.identity(),
                                        self.syncer_state.bitcoin_syncer(),
                                        Request::SyncerTask(task),
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
                                    self.swap_id.bright_blue_italic()
                                );
                                if self.syncer_state.awaiting_funding {
                                    endpoints.send_to(
                                        ServiceBus::Ctl,
                                        self.identity(),
                                        ServiceId::Farcasterd,
                                        Request::FundingCanceled(Blockchain::Monero),
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
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    self.syncer_state.monero_syncer(),
                                    Request::SyncerTask(abort_all.clone()),
                                )?;
                                endpoints.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    self.syncer_state.bitcoin_syncer(),
                                    Request::SyncerTask(abort_all),
                                )?;
                                let swap_success_req = Request::SwapOutcome(Outcome::Refund);
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
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    self.syncer_state.monero_syncer(),
                                    Request::SyncerTask(abort_all.clone()),
                                )?;
                                endpoints.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    self.syncer_state.bitcoin_syncer(),
                                    Request::SyncerTask(abort_all),
                                )?;
                                let swap_success_req = Request::SwapOutcome(Outcome::Buy);
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
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    self.syncer_state.bitcoin_syncer(),
                                    Request::SyncerTask(task),
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
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    self.syncer_state.bitcoin_syncer(),
                                    Request::SyncerTask(task),
                                )?;
                            }

                            TxLabel::Refund if self.state.swap_role() == SwapRole::Bob => {
                                let abort_all = Task::Abort(Abort {
                                    task_target: TaskTarget::AllTasks,
                                    respond: Boolean::False,
                                });
                                endpoints.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    self.syncer_state.monero_syncer(),
                                    Request::SyncerTask(abort_all.clone()),
                                )?;
                                endpoints.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    self.syncer_state.bitcoin_syncer(),
                                    Request::SyncerTask(abort_all),
                                )?;
                                self.state_update(
                                    endpoints,
                                    State::Bob(BobState::FinishB(Outcome::Refund)),
                                )?;
                                let swap_success_req = Request::SwapOutcome(Outcome::Refund);
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
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    self.syncer_state.monero_syncer(),
                                    Request::SyncerTask(abort_all.clone()),
                                )?;
                                endpoints.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    self.syncer_state.bitcoin_syncer(),
                                    Request::SyncerTask(abort_all),
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
                                let swap_success_req = Request::SwapOutcome(Outcome::Punish);
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
                                self.swap_id.bright_blue_italic(),
                                tx_label.bright_white_bold(),
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
                            Request::FundingCanceled(Blockchain::Bitcoin),
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
                        info!("fee: {} sat/kvB", high_priority_sats_per_kvbyte);
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
                                            request: Request::Protocol(Msg::Reveal(
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
                error!("Request is not supported by the CTL interface {}", request);
                return Err(Error::NotSupported(ServiceBus::Ctl, request.get_type()));
            }
        }
        Ok(())
    }
}

impl Runtime {
    pub fn taker_commit(
        &mut self,
        endpoints: &mut Endpoints,
        params: Params,
    ) -> Result<request::Commit, Error> {
        info!(
            "{} | {} to Maker remote peer",
            self.swap_id().bright_blue_italic(),
            "Proposing to take swap".bright_white_bold(),
        );

        let msg = format!(
            "Proposing to take swap {} to Maker remote peer",
            self.swap_id()
        );
        // FIXME: report progress
        // let enquirer = self.enquirer.clone();
        // // Ignoring possible reporting errors here and after: do not want to
        // // halt the swap just because the client disconnected
        // let _ = self.report_progress_message_to(endpoints, &enquirer, msg);

        let engine = CommitmentEngine;
        let commitment = match params {
            Params::Bob(params) => {
                request::Commit::BobParameters(params.commit_bob(self.swap_id(), &engine))
            }
            Params::Alice(params) => {
                request::Commit::AliceParameters(params.commit_alice(self.swap_id(), &engine))
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
    ) -> Result<request::Commit, Error> {
        info!(
            "{} | {} as Maker from Taker through peerd {}",
            swap_id.bright_blue_italic(),
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
                request::Commit::BobParameters(params.commit_bob(self.swap_id(), &engine))
            }
            Params::Alice(params) => {
                request::Commit::AliceParameters(params.commit_alice(self.swap_id(), &engine))
            }
        };

        Ok(commitment)
    }

    pub fn abort_swap(&mut self, endpoints: &mut Endpoints) -> Result<(), Error> {
        let swap_success_req = Request::SwapOutcome(Outcome::Abort);
        self.send_ctl(endpoints, ServiceId::Wallet, swap_success_req.clone())?;
        self.send_ctl(endpoints, ServiceId::Farcasterd, swap_success_req)?;
        info!("{} | Aborted swap.", self.swap_id);
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

pub fn remote_params_candidate(reveal: &Reveal, remote_commit: Commit) -> Result<Params, Error> {
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

fn white_list_ctl_msgs(request: &Request) -> bool {
    match request {
        Request::TakeSwap(_) => true,
        Request::Protocol(Msg::Reveal(Reveal::Proof(_))) => true, // split reveal proof
        Request::Protocol(Msg::Reveal(_)) => true,
        Request::MakeSwap(_) => true,
        Request::FundingUpdated => true,
        Request::Protocol(Msg::CoreArbitratingSetup(_)) => true,
        Request::Protocol(Msg::RefundProcedureSignatures(_)) => true,
        Request::Protocol(Msg::BuyProcedureSignature(_)) => true,
        Request::Tx(_) => true,
        Request::SweepMoneroAddress(_) => true,
        Request::Terminate => true,
        Request::SweepBitcoinAddress(_) => true,
        Request::PeerdReconnected => true,
        Request::Checkpoint(_) => true,
        _ => false,
    }
}

fn white_list_rpc_msgs(request: &Request) -> bool {
    match request {
        Request::AbortSwap => true,
        Request::GetInfo => true,
        _ => false,
    }
}
fn white_list_p2p_msgs(request: &Request) -> bool {
    if let Request::Protocol(msg) = request {
        match msg {
            Msg::MakerCommit(_) => true,
            Msg::TakerCommit(_) => false,
            Msg::Reveal(request::Reveal::Proof(_)) => true,
            Msg::Reveal(_) => true,
            Msg::CoreArbitratingSetup(_) => true,
            Msg::RefundProcedureSignatures(_) => true,
            Msg::BuyProcedureSignature(_) => true,
            Msg::Abort(_) => false,
            Msg::Ping(_) | Msg::Pong(_) | Msg::PingPeer => false,
            _ => false,
        }
    } else {
        false
    }
}
