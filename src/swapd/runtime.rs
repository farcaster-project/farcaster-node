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

use crate::syncerd::{
    opts::Coin, Abort, HeightChanged, SweepAddress, SweepAddressAddendum, SweepSuccess,
    SweepXmrAddress, TaskTarget, WatchHeight, XmrAddressAddendum,
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

use super::storage::{self, Driver};
use crate::rpc::{
    request::{self, Msg},
    Request, ServiceBus,
};
use crate::{Config, CtlServer, Error, LogStyle, Senders, Service, ServiceId};
use bitcoin::{consensus::Encodable, secp256k1};
use bitcoin::{
    hashes::{hex::FromHex, sha256, Hash, HashEngine},
    Txid,
};
use bitcoin::{
    util::{
        bip143::SigHashCache,
        psbt::{serialize::Deserialize, PartiallySignedTransaction},
    },
    Script,
};
use bitcoin::{OutPoint, SigHashType};

use crate::syncerd::types::{
    AddressAddendum, AddressTransaction, Boolean, BroadcastTransaction, BtcAddressAddendum, Event,
    Task, TransactionConfirmations, WatchAddress, WatchTransaction,
};
use farcaster_core::{
    bitcoin::{
        fee::SatPerVByte, segwitv0::LockTx, segwitv0::SegwitV0, timelock::CSVTimelock, Bitcoin,
        BitcoinSegwitV0,
    },
    blockchain::{self, FeeStrategy},
    bundle::{AliceParameters, BobParameters, Proof},
    consensus::{self, Encodable as FarEncodable},
    crypto::{CommitmentEngine, SharedKeyId, TaggedElement},
    monero::{Monero, SHARED_VIEW_KEY_ID},
    negotiation::{Offer, PublicOffer},
    protocol_message::{
        BuyProcedureSignature, CommitAliceParameters, CommitBobParameters, CoreArbitratingSetup,
        RefundProcedureSignatures,
    },
    role::{Arbitrating, SwapRole, TradeRole},
    swap::btcxmr::BtcXmr,
    swap::SwapId,
    transaction::{Broadcastable, Transaction, TxLabel, Witnessable},
};
use internet2::zmqsocket::{self, ZmqSocketAddr, ZmqType};
use internet2::{
    session, CreateUnmarshaller, NodeAddr, Session, TypedEnum, Unmarshall, Unmarshaller,
};
use lnpbp::chain::Chain;
use microservices::esb::{self, Handler};
use monero::{cryptonote::hash::keccak_256, PrivateKey, ViewPair};
use request::{Commit, InitSwap, Params, Reveal, TakeCommit, Tx};

pub fn run(
    config: Config,
    swap_id: SwapId,
    _chain: Chain,
    public_offer: PublicOffer<BtcXmr>,
    local_trade_role: TradeRole,
) -> Result<(), Error> {
    let Offer {
        cancel_timelock,
        punish_timelock,
        maker_role, // SwapRole of maker (Alice or Bob)
        network,
        accordant_amount: monero_amount,
        ..
    } = public_offer.offer;
    // alice or bob
    let local_swap_role = match local_trade_role {
        TradeRole::Maker => maker_role,
        TradeRole::Taker => maker_role.other(),
    };

    let init_state = match local_swap_role {
        SwapRole::Alice => State::Alice(AliceState::StartA(local_trade_role, public_offer)),
        SwapRole::Bob => State::Bob(BobState::StartB(local_trade_role, public_offer)),
    };
    let sweep_monero_thr = match local_swap_role {
        SwapRole::Bob => Some(10),
        SwapRole::Alice => None,
    };
    info!("Initial state: {}", init_state.bright_white_bold());

    let temporal_safety = TemporalSafety {
        cancel_timelock: cancel_timelock.as_u32(),
        punish_timelock: punish_timelock.as_u32(),
        btc_finality_thr: 0,
        race_thr: 3,
        xmr_finality_thr: 0,
        sweep_monero_thr,
    };

    temporal_safety.valid_params()?;
    let tasks = SyncerTasks {
        counter: 0,
        watched_addrs: none!(),
        watched_txs: none!(),
        sweeping_addr: none!(),
    };
    let syncer_state = SyncerState {
        tasks,
        monero_height: 0,
        bitcoin_height: 0,
        confirmation_bound: 50000,
        lock_tx_confs: None,
        cancel_tx_confs: None,
        network,
        bitcoin_syncer: ServiceId::Syncer(Coin::Bitcoin, network),
        monero_syncer: ServiceId::Syncer(Coin::Monero, network),
        monero_amount,
    };

    let runtime = Runtime {
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
        txs: none!(),
        local_params: None,
        remote_params: None,
    };
    let broker = false;
    Service::run(config, runtime, broker)
}

// FIXME: State enum should carry over the data that is accumulated over time,
// and corresponding lines should be removed from Runtime
pub struct Runtime {
    identity: ServiceId,
    peer_service: ServiceId,
    state: State,
    maker_peer: Option<NodeAddr>,
    started: SystemTime,
    enquirer: Option<ServiceId>,
    syncer_state: SyncerState,
    temporal_safety: TemporalSafety,
    pending_requests: HashMap<ServiceId, Vec<PendingRequest>>, // FIXME Something more meaningful than ServiceId to index
    txs: HashMap<TxLabel, bitcoin::Transaction>,
    #[allow(dead_code)]
    storage: Box<dyn storage::Driver>,
    local_params: Option<Params>,  // FIXME this should be removed
    remote_params: Option<Params>, // FIXME this should be removed
}

struct TemporalSafety {
    cancel_timelock: BlockHeight,
    punish_timelock: BlockHeight,
    race_thr: BlockHeight,
    btc_finality_thr: BlockHeight,
    xmr_finality_thr: BlockHeight,
    sweep_monero_thr: Option<BlockHeight>,
}

type BlockHeight = u32;

impl TemporalSafety {
    /// check if temporal params are in correct order
    fn valid_params(&self) -> Result<(), Error> {
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
    fn final_tx(&self, confs: u32, coin: Coin) -> bool {
        let finality_thr = match coin {
            Coin::Bitcoin => self.btc_finality_thr,
            Coin::Monero => self.xmr_finality_thr,
        };
        confs >= finality_thr
    }
    /// lock must be final, valid after lock_minedblock + cancel_timelock
    fn valid_cancel(&self, lock_confirmations: u32) -> bool {
        self.final_tx(lock_confirmations, Coin::Bitcoin)
            && lock_confirmations >= self.cancel_timelock
    }
    /// lock must be final, but buy shall not be raced with cancel
    fn safe_buy(&self, lock_confirmations: u32) -> bool {
        self.final_tx(lock_confirmations, Coin::Bitcoin)
            && lock_confirmations < (self.cancel_timelock - self.race_thr)
    }
    /// cancel must be final, but refund shall not be raced with punish
    fn safe_refund(&self, cancel_confirmations: u32) -> bool {
        self.final_tx(cancel_confirmations, Coin::Bitcoin)
            && cancel_confirmations < (self.punish_timelock - self.race_thr)
    }
    fn valid_punish(&self, cancel_confirmations: u32) -> bool {
        self.final_tx(cancel_confirmations, Coin::Bitcoin)
            && cancel_confirmations >= self.punish_timelock
    }
}

struct SyncerTasks {
    counter: u32,
    watched_txs: HashMap<u32, TxLabel>,
    watched_addrs: HashMap<u32, TxLabel>,
    sweeping_addr: Option<u32>,
}

impl SyncerTasks {
    fn new_taskid(&mut self) -> u32 {
        self.counter += 1;
        self.counter
    }
}

struct SyncerState {
    tasks: SyncerTasks,
    bitcoin_height: u64,
    monero_height: u64,
    confirmation_bound: u32,
    lock_tx_confs: Option<Request>,
    cancel_tx_confs: Option<Request>,
    network: farcaster_core::blockchain::Network,
    bitcoin_syncer: ServiceId,
    monero_syncer: ServiceId,
    monero_amount: monero::Amount,
}

#[derive(Display, Clone)]
pub enum AliceState {
    // #[display("Start: {0:#?} {1:#?}")]
    #[display("Start")]
    StartA(TradeRole, PublicOffer<BtcXmr>), // local, both
    // #[display("Commit: {0}")]
    #[display("Commit")]
    CommitA(CommitC), // local, local, local, remote
    // #[display("Reveal: {0:#?}")]
    #[display("Reveal")]
    RevealA(Params, Commit), // local, remote
    // #[display("RefundProcSigs: {0}")]
    #[display("RefundProcSigs")]
    RefundSigA(RefundSigA),
    #[display("Finish")]
    FinishA,
}

/// Content of Commit state Common to Bob and Alice
#[derive(Clone, Display)]
#[display("{trade_role:#?} {local_params:#?} {local_commit:#?} {remote_commit:#?}")]
pub struct CommitC {
    trade_role: TradeRole,
    local_params: Params,
    local_commit: Commit,
    remote_commit: Option<Commit>,
}

#[derive(Clone, Display)]
#[display("{xmr_locked} and {buy_published}")]
pub struct RefundSigA {
    #[display("xmr_locked({0})")]
    xmr_locked: bool,
    #[display("buy_published({0})")]
    buy_published: bool,
    /* #[display("local_view_share({0})")]
     * local_params: Params */
}

#[derive(Display, Clone)]
pub enum BobState {
    // #[display("Start {0:#?} {1:#?}")]
    #[display("Start")]
    StartB(TradeRole, PublicOffer<BtcXmr>), // local, both
    // #[display("Commit {0} {1}")]
    #[display("Commit")]
    CommitB(CommitC, bitcoin::Address),
    // #[display("Reveal: {0:#?}")]
    #[display("Reveal")]
    RevealB(Params, Commit), // local, remote
    // #[display("CoreArb: {0:#?}")]
    #[display("CoreArb")]
    CorearbB(CoreArbitratingSetup<BtcXmr>), // lock (not signed)
    #[display("BuyProcSig")]
    BuySigB,
    #[display("Finish")]
    FinishB,
}

#[derive(Display, Clone)]
#[display(inner)]
pub enum State {
    #[display("AliceState({0})")]
    Alice(AliceState),
    #[display("BobState({0})")]
    Bob(BobState),
}

// The state impl is not public and may contain code not used yet, we can relax the linter and
// allow dead code.
#[allow(dead_code)]
impl State {
    fn swap_role(&self) -> SwapRole {
        match self {
            State::Alice(_) => SwapRole::Alice,
            State::Bob(_) => SwapRole::Bob,
        }
    }
    fn a_xmr_locked(&self) -> bool {
        if let State::Alice(AliceState::RefundSigA(RefundSigA { xmr_locked, .. })) = self {
            *xmr_locked
        } else {
            false
        }
    }
    fn a_buy_published(&self) -> bool {
        if let State::Alice(AliceState::RefundSigA(RefundSigA { buy_published, .. })) = self {
            *buy_published
        } else {
            false
        }
    }
    fn b_core_arb(&self) -> bool {
        matches!(self, State::Bob(BobState::CorearbB(..)))
    }
    fn b_buy_sig(&self) -> bool {
        matches!(self, State::Bob(BobState::BuySigB))
    }
    fn remote_commit(&self) -> Option<&Commit> {
        match self {
            State::Alice(AliceState::CommitA(CommitC { remote_commit, .. }))
            | State::Bob(BobState::CommitB(CommitC { remote_commit, .. }, _)) => {
                remote_commit.as_ref()
            }
            _ => None,
        }
    }
    fn local_params(&self) -> Option<&Params> {
        match self {
            State::Alice(AliceState::CommitA(CommitC { local_params, .. }))
            | State::Bob(BobState::CommitB(CommitC { local_params, .. }, ..))
            | State::Alice(AliceState::RevealA(local_params, ..))
            | State::Bob(BobState::RevealB(local_params, ..)) => Some(local_params),
            _ => None,
        }
    }
    fn puboffer(&self) -> Option<&PublicOffer<BtcXmr>> {
        match self {
            State::Alice(AliceState::StartA(_, puboffer))
            | State::Bob(BobState::StartB(_, puboffer)) => Some(puboffer),
            _ => None,
        }
    }
    fn b_address(&self) -> Option<&bitcoin::Address> {
        match self {
            State::Bob(BobState::CommitB(_, addr)) => Some(addr),
            _ => None,
        }
    }
    fn commit(&self) -> bool {
        matches!(
            self,
            State::Alice(AliceState::CommitA(..)) | State::Bob(BobState::CommitB(..))
        )
    }
    fn reveal(&self) -> bool {
        matches!(
            self,
            State::Alice(AliceState::RevealA(..)) | State::Bob(BobState::RevealB(..))
        )
    }
    fn a_refundsig(&self) -> bool {
        matches!(self, State::Alice(AliceState::RefundSigA(..)))
    }
    fn start(&self) -> bool {
        matches!(
            self,
            State::Alice(AliceState::StartA(..)) | State::Bob(BobState::StartB(..))
        )
    }
    fn trade_role(&self) -> Option<TradeRole> {
        match self {
            State::Alice(AliceState::StartA(trade_role, ..))
            | State::Bob(BobState::StartB(trade_role, ..))
            | State::Alice(AliceState::CommitA(CommitC { trade_role, .. }))
            | State::Bob(BobState::CommitB(CommitC { trade_role, .. }, ..)) => Some(*trade_role),
            _ => None,
        }
    }
    fn sup_start_to_commit(
        self,
        local_commit: Commit,
        local_params: Params,
        funding_address: Option<bitcoin::Address>,
        remote_commit: Option<Commit>,
    ) -> Self {
        if !self.start() {
            error!("Not on Start state, not updating state");
            return self;
        }
        match (self, funding_address) {
            (State::Bob(BobState::StartB(trade_role, _)), Some(addr)) => {
                State::Bob(BobState::CommitB(
                    CommitC {
                        trade_role,
                        local_params,
                        local_commit,
                        remote_commit,
                    },
                    addr,
                ))
            }
            (State::Alice(AliceState::StartA(trade_role, _)), None) => {
                State::Alice(AliceState::CommitA(CommitC {
                    trade_role,
                    local_params,
                    local_commit,
                    remote_commit,
                }))

            }
            _ => unreachable!(
                "state conditional enforces state is Start: Expects Start state, and funding_address"
            ),
        }
    }
    fn sup_commit_to_reveal(self, remote_commit: Commit) -> Self {
        if !self.commit() {
            error!("Not on Commit state, not updating state");
            return self;
        }

        match self {
            State::Alice(AliceState::CommitA(CommitC { local_params, .. })) => {
                State::Alice(AliceState::RevealA(local_params, remote_commit))
            }
            State::Bob(BobState::CommitB(
                CommitC {
                    local_params,
                    remote_commit: None,
                    ..
                },
                ..,
            )) => State::Bob(BobState::RevealB(local_params, remote_commit)),

            _ => unreachable!("checked state on pattern to be Commit"),
        }
    }
}

impl CtlServer for Runtime {}

impl esb::Handler<ServiceBus> for Runtime {
    type Request = Request;
    type Address = ServiceId;
    type Error = Error;

    fn identity(&self) -> ServiceId {
        self.identity.clone()
    }

    fn handle(
        &mut self,
        senders: &mut esb::SenderList<ServiceBus, ServiceId>,
        bus: ServiceBus,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Self::Error> {
        match bus {
            ServiceBus::Msg => self.handle_rpc_msg(senders, source, request),
            ServiceBus::Ctl => self.handle_rpc_ctl(senders, source, request),
            _ => Err(Error::NotSupported(ServiceBus::Bridge, request.get_type())),
        }
    }

    fn handle_err(&mut self, _: esb::Error) -> Result<(), esb::Error> {
        // We do nothing and do not propagate error; it's already being reported
        // with `error!` macro by the controller. If we propagate error here
        // this will make whole daemon panic
        Ok(())
    }
}

impl SyncerState {
    fn task_lifetime(&self, coin: Coin) -> u64 {
        let height = self.height(coin);
        if height > 0 {
            height + 500
        } else {
            u64::MAX
        }
    }
    fn bitcoin_syncer(&self) -> ServiceId {
        self.bitcoin_syncer.clone()
    }
    fn monero_syncer(&self) -> ServiceId {
        self.monero_syncer.clone()
    }
    fn height(&self, coin: Coin) -> u64 {
        match coin {
            Coin::Bitcoin => self.bitcoin_height,
            Coin::Monero => self.monero_height,
        }
    }
    fn handle_height_change(&mut self, new_height: u64, coin: Coin) {
        let height = match coin {
            Coin::Bitcoin => &mut self.bitcoin_height,
            Coin::Monero => &mut self.monero_height,
        };
        if &new_height > height {
            info!("{:?} new height {}", coin, &new_height);
            *height = new_height;
        } else {
            warn!("block height did not increment, maybe syncer sends multiple events");
        }
    }
    fn watch_tx_btc(&mut self, txid: Txid, tx_label: TxLabel) -> Task {
        let id = self.tasks.new_taskid();
        self.tasks.watched_txs.insert(id, tx_label);
        info!(
            "Watching tx {} {}",
            tx_label.bright_green_bold(),
            txid.addr()
        );
        Task::WatchTransaction(WatchTransaction {
            id,
            lifetime: self.task_lifetime(Coin::Bitcoin),
            hash: txid.to_vec(),
            confirmation_bound: self.confirmation_bound,
        })
    }

    fn watch_tx_xmr(&mut self, hash: Vec<u8>, tx_label: TxLabel) -> Task {
        let id = self.tasks.new_taskid();
        self.tasks.watched_txs.insert(id, tx_label);
        info!(
            "Watching tx {} {} with id {}",
            tx_label.bright_green_bold(),
            hex::encode(&hash).addr(),
            id
        );
        Task::WatchTransaction(WatchTransaction {
            id,
            lifetime: self.task_lifetime(Coin::Monero),
            hash,
            confirmation_bound: self.confirmation_bound,
        })
    }
    fn watch_addr_btc(&mut self, script_pubkey: Script, tx_label: TxLabel) -> Task {
        let id = self.tasks.new_taskid();
        let from_height = self.height(Coin::Bitcoin);
        self.tasks.watched_addrs.insert(id, tx_label);
        info!(
            "Watching {} address with {}",
            tx_label.bright_green_bold(),
            script_pubkey
        );
        let addendum = BtcAddressAddendum {
            address: None,
            from_height,
            script_pubkey,
        };
        Task::WatchAddress(WatchAddress {
            id,
            lifetime: self.task_lifetime(Coin::Bitcoin),
            addendum: AddressAddendum::Bitcoin(addendum),
            include_tx: Boolean::True,
        })
    }

    fn watch_addr_xmr(
        &mut self,
        spend: monero::PublicKey,
        view: monero::PrivateKey,
        tx_label: TxLabel,
    ) -> Task {
        info!("XMR view key: {}", view);
        info!("XMR spend key: {}", spend);
        let viewpair = monero::ViewPair { spend, view };
        let address = monero::Address::from_viewpair(self.network.into(), &viewpair);

        let from_height = self.height(Coin::Monero);

        let addendum = AddressAddendum::Monero(XmrAddressAddendum {
            spend_key: spend,
            view_key: view,
            from_height,
        });

        let id = self.tasks.new_taskid();
        self.tasks.watched_addrs.insert(id, tx_label);

        info!(
            "Watching address {} {}",
            tx_label.bright_green_bold(),
            address.addr()
        );

        let watch_addr = WatchAddress {
            id,
            lifetime: self.task_lifetime(Coin::Monero),
            addendum,
            include_tx: Boolean::False,
        };
        Task::WatchAddress(watch_addr)
    }
    fn sweep_xmr(
        &mut self,
        view_key: monero::PrivateKey,
        spend_key: monero::PrivateKey,
        address: monero::Address,
    ) -> Task {
        let id = self.tasks.new_taskid();
        self.tasks.sweeping_addr = Some(id);
        let lifetime = self.task_lifetime(Coin::Monero);
        let addendum = SweepAddressAddendum::Monero(SweepXmrAddress {
            view_key,
            spend_key,
            address,
        });
        let sweep_task = SweepAddress {
            id,
            lifetime,
            addendum,
        };
        Task::SweepAddress(sweep_task)
    }

    fn acc_lock_watched(&self) -> bool {
        self.tasks
            .watched_addrs
            .values()
            .any(|&x| x == TxLabel::AccLock)
    }
    fn handle_tx_confs(&self, id: &u32, confirmations: &Option<u32>) {
        if let Some(txlabel) = self.tasks.watched_txs.get(id) {
            match confirmations {
                Some(0) => {
                    info!(
                        "tx {} on mempool but hasn't been mined",
                        txlabel.bright_green_bold()
                    );
                }
                Some(confs) => {
                    info!(
                        "tx {} mined with {} confirmations",
                        txlabel.bright_green_bold(),
                        confs.bright_green_bold(),
                    )
                }
                None => {
                    info!("tx {} not on the mempool", txlabel.bright_green_bold());
                }
            }
        } else {
            error!(
                "received event with unknown transaction and task id {}",
                &id
            )
        }
    }
}
impl Runtime {
    fn send_peer(&self, senders: &mut Senders, msg: request::Msg) -> Result<(), Error> {
        trace!("sending peer message {}", msg.bright_yellow_bold());
        senders.send_to(
            ServiceBus::Msg,
            self.identity(),
            self.peer_service.clone(), // = ServiceId::Loopback
            Request::Protocol(msg),
        )?;
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

    fn state_update(&mut self, senders: &mut Senders, next_state: State) -> Result<(), Error> {
        let msg = format!(
            "State transition: {} -> {}",
            self.state.bright_white_bold(),
            next_state.bright_white_bold()
        );
        info!("{}", &msg);
        self.state = next_state;
        self.report_success_to(senders, self.enquirer.clone(), Some(msg))?;
        Ok(())
    }

    fn broadcast(
        &mut self,
        tx: bitcoin::Transaction,
        tx_label: TxLabel,
        senders: &mut Senders,
    ) -> Result<(), Error> {
        let req = Request::SyncerTask(Task::BroadcastTransaction(BroadcastTransaction {
            id: self.syncer_state.tasks.new_taskid(),
            tx: bitcoin::consensus::serialize(&tx),
        }));

        info!(
            "Broadcasting {} tx {}",
            tx_label.bright_green_bold(),
            tx.txid().addr()
        );
        Ok(senders.send_to(
            ServiceBus::Ctl,
            self.identity(),
            self.syncer_state.bitcoin_syncer(),
            req,
        )?)
    }

    fn handle_rpc_msg(
        &mut self,
        senders: &mut Senders,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        if self.peer_service != source {
            return Err(Error::Farcaster(format!(
                "{}: expected {}, found {}",
                "Incorrect peer connection", self.peer_service, source
            )));
        }
        let msg_bus = ServiceBus::Msg;
        match &request {
            Request::Protocol(msg) => {
                if msg.swap_id() != self.swap_id() {
                    return Err(Error::Farcaster(format!(
                        "{}: expected {}, found {}",
                        "Incorrect swap_id ",
                        self.swap_id(),
                        msg.swap_id(),
                    )));
                }
                match &msg {
                    // we are taker and the maker committed, now we reveal after checking
                    // whether we're Bob or Alice and that we're on a compatible state
                    Msg::MakerCommit(remote_commit)
                        if self.state.commit()
                            && self.state.trade_role() == Some(TradeRole::Taker) =>
                    {
                        trace!("received commitment from counterparty, can now reveal");
                        let next_state = self
                            .state
                            .clone()
                            .sup_commit_to_reveal(remote_commit.clone());

                        match self.state.swap_role() {
                            SwapRole::Alice => {}
                            SwapRole::Bob => {
                                let addr = self
                                    .state
                                    .b_address()
                                    .cloned()
                                    .expect("address available at CommitB");
                                let msg = format!(
                                    "{} {}",
                                    "Funding address:".bright_white_bold(),
                                    addr.bright_yellow_bold()
                                );
                                let enquirer = self.enquirer.clone();
                                let _ = self.report_progress_to(senders, &enquirer, msg);

                                let task = self
                                    .syncer_state
                                    .watch_addr_btc(addr.script_pubkey(), TxLabel::Funding);
                                self.send_ctl(
                                    senders,
                                    self.syncer_state.bitcoin_syncer(),
                                    Request::SyncerTask(task),
                                )?;
                            }
                        }

                        trace!("Watch height bitcoin");
                        let watch_height_bitcoin = Task::WatchHeight(WatchHeight {
                            id: self.syncer_state.tasks.new_taskid(),
                            lifetime: self.syncer_state.task_lifetime(Coin::Bitcoin),
                        });
                        senders.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            self.syncer_state.bitcoin_syncer(),
                            Request::SyncerTask(watch_height_bitcoin),
                        )?;

                        trace!("Watch height monero");
                        let watch_height_monero = Task::WatchHeight(WatchHeight {
                            id: self.syncer_state.tasks.new_taskid(),
                            lifetime: self.syncer_state.task_lifetime(Coin::Monero),
                        });
                        senders.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            self.syncer_state.monero_syncer(),
                            Request::SyncerTask(watch_height_monero),
                        )?;

                        self.send_wallet(msg_bus, senders, request)?;
                        self.state_update(senders, next_state)?;
                    }
                    Msg::TakerCommit(_) => {
                        unreachable!(
                            "msg handled by farcasterd/walletd, and indirectly here by \
                             Ctl Request::MakeSwap"
                        )
                    }
                    Msg::Reveal(Reveal::Proof(_)) => {
                        // These messages are saved as pending if Bob and then forwarded once the
                        // parameter reveal forward is triggered. If Alice, send immediately.
                        match self.state.swap_role() {
                            SwapRole::Bob => {
                                let pending_request = PendingRequest {
                                    request,
                                    dest: ServiceId::Wallet,
                                    bus_id: ServiceBus::Msg,
                                };
                                trace!("Added pending request to be forwarded later",);
                                if self
                                    .pending_requests
                                    .insert(ServiceId::Wallet, vec![pending_request])
                                    .is_some()
                                {
                                    error!(
                                        "Pending requests already existed prior to Reveal::Proof!"
                                    )
                                }
                            }
                            SwapRole::Alice => {
                                info!("Alice: forwarding reveal");
                                trace!(
                                    "sending request {} to {} on bus {}",
                                    &request,
                                    &ServiceId::Wallet,
                                    &ServiceBus::Msg
                                );
                                self.send_wallet(msg_bus, senders, request)?
                            }
                        }
                    }
                    // bob and alice
                    // store parameters from counterparty if we have not received them yet.
                    // if we're maker, also reveal to taker if their commitment is valid.
                    Msg::Reveal(reveal) => {
                        // TODO: since we're not actually revealing, find other name for
                        // intermediary state
                        let (next_state, remote_commit) = match self.state.clone() {
                            // counterparty has already revealed commitment, i.e. we're
                            // maker and counterparty is taker. now proceed to reveal state.
                            State::Alice(AliceState::CommitA(CommitC {
                                remote_commit: Some(remote_commit),
                                local_params,
                                ..
                            })) => Ok((
                                State::Alice(AliceState::RevealA(
                                    local_params,
                                    remote_commit.clone(),
                                )),
                                remote_commit,
                            )),
                            State::Bob(BobState::CommitB(
                                CommitC {
                                    remote_commit: Some(remote_commit),
                                    local_params,
                                    ..
                                },
                                addr,
                            )) => {
                                let msg = format!(
                                    "{} {}",
                                    "Funding address:".bright_white_bold(),
                                    addr.bright_yellow_bold()
                                );
                                let enquirer = self.enquirer.clone();
                                let _ = self.report_progress_to(senders, &enquirer, msg);

                                let watch_addr_task = self
                                    .syncer_state
                                    .watch_addr_btc(addr.script_pubkey(), TxLabel::Funding);
                                self.send_ctl(
                                    senders,
                                    self.syncer_state.bitcoin_syncer(),
                                    Request::SyncerTask(watch_addr_task),
                                )?;
                                Ok((
                                    State::Bob(BobState::RevealB(
                                        local_params,
                                        remote_commit.clone(),
                                    )),
                                    remote_commit,
                                ))
                            }
                            // we're already in reveal state, i.e. we're taker, so don't change
                            // state once counterparty reveals too.
                            State::Alice(AliceState::RevealA(local_params, remote_commit)) => Ok((
                                State::Alice(AliceState::RevealA(
                                    local_params,
                                    remote_commit.clone(),
                                )),
                                remote_commit,
                            )),
                            State::Bob(BobState::RevealB(local_params, remote_commit)) => Ok((
                                State::Bob(BobState::RevealB(local_params, remote_commit.clone())),
                                remote_commit,
                            )),
                            state => Err(Error::Farcaster(format!(
                                "Must be on Commit or Reveal state, found {}",
                                state
                            ))),
                        }?;

                        // parameter processing irrespective of maker & taker role
                        let core_wallet = CommitmentEngine;
                        let remote_params_candidate = match reveal {
                            Reveal::AliceParameters(reveal) => match &remote_commit {
                                Commit::AliceParameters(commit) => {
                                    commit.verify_with_reveal(&core_wallet, reveal.clone())?;
                                    Some(Params::Alice(reveal.clone().into()))
                                }
                                _ => {
                                    let err_msg = "expected Some(Commit::Alice(commit))";
                                    error!("{}", err_msg);
                                    return Err(Error::Farcaster(err_msg.to_string()));
                                }
                            },
                            Reveal::BobParameters(reveal) => match &remote_commit {
                                Commit::BobParameters(commit) => {
                                    commit.verify_with_reveal(&core_wallet, reveal.clone())?;
                                    Some(Params::Bob(reveal.clone().into()))
                                }
                                _ => {
                                    let err_msg = "expected Some(Commit::Bob(commit))";
                                    error!("{}", err_msg);
                                    return Err(Error::Farcaster(err_msg.to_string()));
                                }
                            },
                            Reveal::Proof(_) => {
                                error!("this should have been caught by another pattern!");
                                None
                            }
                        };
                        if remote_params_candidate.is_some() {
                            info!("{:?} sets remote_params", self.state.swap_role());
                            self.remote_params = remote_params_candidate
                        }

                        // pass request on to wallet daemon so that it can set remote params
                        match self.state {
                            // validated state above, no need to check again
                            State::Alice(..) => {
                                // Alice already sends RevealProof immediately, so only have to
                                // forward Reveal now
                                trace!(
                                    "sending request {} to {} on bus {}",
                                    &request,
                                    &ServiceId::Wallet,
                                    &ServiceBus::Msg
                                );
                                self.send_wallet(msg_bus, senders, request)?
                            }
                            State::Bob(..) => {
                                // sending this request will initialize the
                                // arbitrating setup, that can be only performed
                                // after the funding tx was seen
                                let pending_request = PendingRequest {
                                    request,
                                    dest: ServiceId::Wallet,
                                    bus_id: ServiceBus::Msg,
                                };

                                trace!(
                                    "This pending request will be called later: {:?}",
                                    &pending_request
                                );
                                let pending_requests = self.pending_requests.get_mut(&ServiceId::Wallet)
                                    .expect("should already have received Reveal::Proof, so this key should exist.");
                                if pending_requests.len() != 1 {
                                    error!("should have a single pending Reveal::Proof only FIXME")
                                }
                                pending_requests.push(pending_request);
                            }
                        }
                        // up to here for both maker and taker, following only Maker

                        // if did not yet reveal, maker only. on the msg flow as
                        // of 2021-07-13 taker reveals first
                        if self.state.commit() && self.state.trade_role() == Some(TradeRole::Maker)
                        {
                            trace!("Watch height bitcoin");
                            let watch_height_bitcoin = Task::WatchHeight(WatchHeight {
                                id: self.syncer_state.tasks.new_taskid(),
                                lifetime: self.syncer_state.task_lifetime(Coin::Bitcoin),
                            });
                            senders.send_to(
                                ServiceBus::Ctl,
                                self.identity(),
                                self.syncer_state.bitcoin_syncer(),
                                Request::SyncerTask(watch_height_bitcoin),
                            )?;

                            trace!("Watch height monero");
                            let watch_height_monero = Task::WatchHeight(WatchHeight {
                                id: self.syncer_state.tasks.new_taskid(),
                                lifetime: self.syncer_state.task_lifetime(Coin::Monero),
                            });
                            senders.send_to(
                                ServiceBus::Ctl,
                                self.identity(),
                                self.syncer_state.monero_syncer(),
                                Request::SyncerTask(watch_height_monero),
                            )?;
                            self.state_update(senders, next_state)?;
                        } else {
                            debug!(
                                "You are the Taker, which revealed already, nothing to reveal. \
                                    Or state is not Commit, state {}",
                                &self.state
                            );
                        }
                    }
                    // alice receives, bob sends
                    Msg::CoreArbitratingSetup(CoreArbitratingSetup {
                        swap_id,
                        lock,
                        cancel,
                        refund,
                        ..
                    }) if self.state.swap_role() == SwapRole::Alice
                        && self.state.reveal()
                        && swap_id == &self.swap_id() =>
                    {
                        for (&tx, tx_label) in [lock, cancel, refund].iter().zip([
                            TxLabel::Lock,
                            TxLabel::Cancel,
                            TxLabel::Refund,
                        ]) {
                            let txid = tx.clone().extract_tx().txid();
                            let task = self.syncer_state.watch_tx_btc(txid, tx_label);
                            senders.send_to(
                                ServiceBus::Ctl,
                                self.identity(),
                                self.syncer_state.bitcoin_syncer(),
                                Request::SyncerTask(task),
                            )?;
                        }
                        self.send_wallet(msg_bus, senders, request)?;
                    }
                    // bob receives, alice sends
                    Msg::RefundProcedureSignatures(_) if self.state.b_core_arb() => {
                        self.send_wallet(msg_bus, senders, request)?;
                    }
                    // alice receives, bob sends
                    Msg::BuyProcedureSignature(BuyProcedureSignature { buy, .. })
                        if self.state.a_refundsig() =>
                    {
                        // Alice verifies that she has sent refund procedure signatures before
                        // processing the buy signatures from Bob
                        let txid = buy.clone().extract_tx().txid();
                        let task = self.syncer_state.watch_tx_btc(txid, TxLabel::Buy);
                        senders.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            self.syncer_state.bitcoin_syncer(),
                            Request::SyncerTask(task),
                        )?;
                        self.send_wallet(msg_bus, senders, request)?
                    }

                    // bob and alice
                    Msg::Abort(_) => {
                        return Err(Error::Farcaster("Abort not yet supported".to_string()))
                    }
                    Msg::Ping(_) | Msg::Pong(_) | Msg::PingPeer => {
                        unreachable!("ping/pong must remain in peerd, and unreachable in swapd")
                    }
                    request => error!("request not supported {}", request),
                }
            }
            // let _ = self.report_progress_to(senders, &enquirer, msg);
            // let _ = self.report_success_to(senders, &enquirer, Some(msg));
            _ => {
                error!("MSG RPC can be only used for forwarding farcaster protocol messages");
                return Err(Error::NotSupported(ServiceBus::Msg, request.get_type()));
            }
        }
        Ok(())
    }

    fn handle_rpc_ctl(
        &mut self,
        senders: &mut Senders,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        match (&request, &source) {

            (Request::Hello, _) => {
                info!("Source: {} is connected", source);
            }
            (_, ServiceId::Syncer(..)) if source == self.syncer_state.bitcoin_syncer || source == self.syncer_state.monero_syncer => {
            }
            (
                _,
                ServiceId::Farcasterd
                | ServiceId::Wallet
            ) => {}
            (Request::GetInfo, ServiceId::Client(_)) => {}
            _ => return Err(Error::Farcaster(
                "Permission Error: only Farcasterd, Wallet, Client and Syncer can can control swapd"
                    .to_string(),
            )),
        };

        match request {
            Request::SweepXmrAddress(SweepXmrAddress {
                view_key,
                spend_key,
                address,
            }) if source == ServiceId::Wallet => {
                let task = self.syncer_state.sweep_xmr(view_key, spend_key, address);
                let request = Request::SyncerTask(task);
                let dest = self.syncer_state.monero_syncer();
                let pending_request = PendingRequest {
                    request,
                    dest: dest.clone(),
                    bus_id: ServiceBus::Ctl,
                };
                if self
                    .pending_requests
                    .insert(dest, vec![pending_request])
                    .is_some()
                {
                    error!("pending request for syncer already there")
                }
            }
            Request::TakeSwap(InitSwap {
                peerd,
                report_to,
                local_params,
                swap_id,
                remote_commit: None,
                funding_address, // Some(_) for Bob, None for Alice
            }) if self.state.start() => {
                if ServiceId::Swap(swap_id) != self.identity {
                    error!(
                        "{}: {}",
                        "This swapd instance is not reponsible for swap_id", swap_id
                    );
                    return Ok(());
                };
                self.peer_service = peerd.clone();
                self.enquirer = report_to.clone();
                self.local_params = Some(local_params.clone());

                if let ServiceId::Peer(ref addr) = peerd {
                    self.maker_peer = Some(addr.clone());
                }
                let local_commit =
                    self.taker_commit(senders, local_params.clone())
                        .map_err(|err| {
                            self.report_failure_to(
                                senders,
                                &report_to,
                                microservices::rpc::Failure {
                                    code: 0, // TODO: Create error type system
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
                let public_offer = self
                    .state
                    .puboffer()
                    .map(|offer| offer.to_string())
                    .expect("state Start has puboffer");
                let take_swap = TakeCommit {
                    commit: local_commit,
                    public_offer,
                    swap_id,
                };
                self.send_peer(senders, Msg::TakerCommit(take_swap))?;
                self.state_update(senders, next_state)?;
            }
            Request::Protocol(Msg::Reveal(reveal)) if self.state.reveal() => {
                let local_params = self
                    .state
                    .local_params()
                    .expect("reveal state has local_params");
                let reveal_proof = Msg::Reveal(reveal);
                let swap_id = reveal_proof.swap_id();
                self.send_peer(senders, reveal_proof)?;
                trace!("sent reveal_proof to peerd");
                let reveal_params: Reveal = (swap_id, local_params.clone()).into();
                self.send_peer(senders, Msg::Reveal(reveal_params))?;
                trace!("sent reveal_params to peerd");
            }

            Request::MakeSwap(InitSwap {
                peerd,
                report_to,
                local_params,
                swap_id,
                remote_commit: Some(remote_commit),
                funding_address, // Some(_) for Bob, None for Alice
            }) if self.state.start() => {
                self.peer_service = peerd.clone();
                if let ServiceId::Peer(ref addr) = peerd {
                    self.maker_peer = Some(addr.clone());
                }
                self.enquirer = report_to.clone();
                self.local_params = Some(local_params.clone());
                let local_commit = self
                    .maker_commit(senders, &peerd, swap_id, &local_params)
                    .map_err(|err| {
                        self.report_failure_to(
                            senders,
                            &report_to,
                            microservices::rpc::Failure {
                                code: 0, // TODO: Create error type system
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
                self.send_peer(senders, Msg::MakerCommit(local_commit))?;
                self.state_update(senders, next_state)?;
            }
            Request::FundingUpdated
                if source == ServiceId::Wallet
                    && self.state.reveal()
                    && self.pending_requests.contains_key(&source)
                    && self
                        .pending_requests
                        .get(&source)
                        .map(|reqs| reqs.len() == 2)
                        .unwrap() =>
            {
                trace!("funding updated received from wallet");
                let mut pending_requests = self
                    .pending_requests
                    .remove(&source)
                    .expect("checked above, should have pending Reveal{Proof} requests");
                let PendingRequest {
                    request: request_parameters,
                    dest: dest_parameters,
                    bus_id: bus_id_parameters,
                } = pending_requests.pop().expect("checked .len() == 2");
                let PendingRequest {
                    request: request_proof,
                    dest: dest_proof,
                    bus_id: bus_id_proof,
                } = pending_requests.pop().expect("checked .len() == 2");
                // continue RevealProof
                // continuing request by sending it to wallet
                if let (
                    Request::Protocol(Msg::Reveal(Reveal::Proof(_))),
                    ServiceId::Wallet,
                    ServiceBus::Msg,
                ) = (&request_proof, &dest_proof, &bus_id_proof)
                {
                    trace!(
                        "sending request {} to {} on bus {}",
                        &request_proof,
                        &dest_proof,
                        &bus_id_proof
                    );
                    senders.send_to(bus_id_proof, self.identity(), dest_proof, request_proof)?
                } else {
                    error!("Not the expected request: found {:?}", request);
                }

                // continue Reveal
                // continuing request by sending it to wallet
                if let (
                    Request::Protocol(Msg::Reveal(Reveal::AliceParameters(_))),
                    ServiceId::Wallet,
                    ServiceBus::Msg,
                ) = (&request_parameters, &dest_parameters, &bus_id_parameters)
                {
                    trace!(
                        "sending request {} to {} on bus {}",
                        &request_parameters,
                        &dest_parameters,
                        &bus_id_parameters
                    );
                    senders.send_to(
                        bus_id_parameters,
                        self.identity(),
                        dest_parameters,
                        request_parameters,
                    )?
                } else {
                    error!("Not the expected request: found {:?}", request);
                }
            }
            // Request::SyncerEvent(ref event) => match (&event, source) {
            // handle monero events here
            // }
            Request::SyncerEvent(ref event) if source == self.syncer_state.monero_syncer => {
                match &event {
                    Event::HeightChanged(HeightChanged { height, .. }) => {
                        self.syncer_state
                            .handle_height_change(*height, Coin::Monero);
                    }
                    Event::AddressTransaction(AddressTransaction {
                        id,
                        hash,
                        amount,
                        block,
                        tx,
                    }) if self.state.swap_role() == SwapRole::Alice => {
                        info!(
                            "Event details: {} {:?} {} {:?} {:?}",
                            id, hash, amount, block, tx
                        );
                        if let State::Alice(AliceState::RefundSigA(RefundSigA {
                            xmr_locked, ..
                        })) = &mut self.state
                        {
                            if !*xmr_locked {
                                warn!("setting xmr_locked");
                                *xmr_locked = true;
                            } else {
                                warn!("xmr_locked was already set to true")
                            }
                        }
                        let task = self
                            .syncer_state
                            .watch_tx_xmr(hash.clone(), TxLabel::AccLock);
                        senders.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            self.syncer_state.monero_syncer(),
                            Request::SyncerTask(task),
                        )?;
                    }
                    Event::AddressTransaction(AddressTransaction {
                        id,
                        hash,
                        amount,
                        block: _,
                        tx: _,
                    }) if self.state.swap_role() == SwapRole::Bob => {
                        if amount < &self.syncer_state.monero_amount.as_pico() {
                            error!(
                                "Not enough monero locked: expected {}, found {}",
                                self.syncer_state.monero_amount, amount
                            );
                            return Ok(());
                        }
                        if let Some(tx_label) = self.syncer_state.tasks.watched_addrs.remove(id) {
                            let watch_tx = self.syncer_state.watch_tx_xmr(hash.clone(), tx_label);
                            senders.send_to(
                                ServiceBus::Ctl,
                                self.identity(),
                                self.syncer_state.monero_syncer(),
                                Request::SyncerTask(watch_tx),
                            )?;
                        }
                    }
                    Event::TransactionConfirmations(TransactionConfirmations {
                        id: _,
                        block: _,
                        confirmations: Some(confirmations),
                    }) if self.state.b_buy_sig()
                        && *confirmations
                            >= self.temporal_safety.sweep_monero_thr.expect(
                                "buysig is bob's state, and bob sets his sweep_monero_thr at launch",
                            )
                        && self.pending_requests.contains_key(&source) =>
                    {
                        let PendingRequest {
                            request,
                            dest,
                            bus_id,
                        } = self
                            .pending_requests
                            .remove(&source)
                            .expect("Checked above")
                            .pop()
                            .unwrap();
                        if let (Request::SyncerTask(Task::SweepAddress(..)), ServiceBus::Ctl) =
                            (&request, &bus_id)
                        {
                            info!("sweeping monero");
                            senders.send_to(bus_id, self.identity(), dest, request)?;
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
                    }) if self.temporal_safety.final_tx(*confirmations, Coin::Monero)
                        && self.state.b_core_arb()
                        && self.pending_requests.contains_key(&source)
                        && self.pending_requests.get(&source).map(|reqs| reqs.len() == 1).unwrap()
                        => {
                        // error!("not checking tx rcvd is accordant lock");
                        let PendingRequest {
                            request,
                            dest,
                            bus_id,
                        } = self
                            .pending_requests
                            .remove(&source)
                            .expect("Checked above")
                            .pop()
                            .unwrap();
                        if let (Request::Protocol(Msg::BuyProcedureSignature(_)), ServiceBus::Msg) =
                            (&request, &bus_id)
                        {
                            senders.send_to(bus_id, self.identity(), dest, request)?;
                            info!("sent buyproceduresignature at state {}", &self.state);
                            let next_state = State::Bob(BobState::BuySigB);
                            self.state_update(senders, next_state)?;
                        } else {
                            error!(
                                "Not buyproceduresignatures {} or not Msg bus found {}",
                                request, bus_id
                            );
                        }
                    }
                    Event::TransactionConfirmations(TransactionConfirmations {
                        id,
                        block: _,
                        confirmations,
                    }) if self.syncer_state.tasks.watched_txs.contains_key(id) => {
                        self.syncer_state.handle_tx_confs(id, confirmations);
                    }
                    Event::TaskAborted(_) => {}
                    Event::SweepSuccess(SweepSuccess { id, .. })
                        if self.state.b_buy_sig()
                            && self.syncer_state.tasks.sweeping_addr.is_some()
                            && &self.syncer_state.tasks.sweeping_addr.unwrap() == id =>
                    {
                        self.state_update(senders, State::Bob(BobState::FinishB))?;
                        let abort_all = Task::Abort(Abort {
                            task_target: TaskTarget::AllTasks,
                        });
                        senders.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            self.syncer_state.monero_syncer(),
                            Request::SyncerTask(abort_all.clone()),
                        )?;
                        senders.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            self.syncer_state.bitcoin_syncer(),
                            Request::SyncerTask(abort_all),
                        )?;
                        let swap_success_req = Request::SwapSuccess(true);
                        self.send_wallet(
                            ServiceBus::Ctl,
                            senders,
                            swap_success_req.clone(),
                        )?;
                        self.send_ctl(senders, ServiceId::Farcasterd, swap_success_req)?;
                        // remove txs to invalidate outdated states
                        self.txs.remove(&TxLabel::Cancel);
                        self.txs.remove(&TxLabel::Refund);
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
                            .handle_height_change(*height, Coin::Bitcoin);
                    }
                    Event::AddressTransaction(AddressTransaction {
                        id,
                        hash: _,
                        amount: _,
                        block: _,
                        tx,
                    }) if self.syncer_state.tasks.watched_addrs.get(id).is_some() => {
                        let tx = bitcoin::Transaction::deserialize(tx)?;
                        trace!(
                            "Received AddressTransaction, processing tx {}",
                            &tx.txid().addr()
                        );
                        let txlabel = self.syncer_state.tasks.watched_addrs.get(id).unwrap();
                        match txlabel {
                            TxLabel::Funding => {
                                log_tx_seen(txlabel, &tx.txid());
                                let req = Request::Tx(Tx::Funding(tx));
                                self.send_wallet(ServiceBus::Ctl, senders, req)?;
                            }
                            TxLabel::Buy if self.state.b_buy_sig() => {
                                log_tx_seen(txlabel, &tx.txid());
                                let req = Request::Tx(Tx::Buy(tx));
                                self.send_wallet(ServiceBus::Ctl, senders, req)?
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
                                    && !self.state.a_buy_published() =>
                            {
                                log_tx_seen(txlabel, &tx.txid());
                                let req = Request::Tx(Tx::Refund(tx));
                                self.send_wallet(ServiceBus::Ctl, senders, req)?
                            }
                            txlabel => {
                                error!("address transaction event not supported for tx {}", txlabel)
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
                    Event::TransactionConfirmations(TransactionConfirmations {
                        id,
                        block: _,
                        confirmations: Some(confirmations),
                    }) if self.temporal_safety.final_tx(*confirmations, Coin::Bitcoin)
                        && self.syncer_state.tasks.watched_txs.get(id).is_some() =>
                    {
                        self.syncer_state.handle_tx_confs(id, &Some(*confirmations));
                        let txlabel = self.syncer_state.tasks.watched_txs.get(id).unwrap();
                        info!(
                            "tx {} final: {} confirmations",
                            txlabel.bright_green_bold(),
                            confirmations.bright_green_bold()
                        );
                        // saving requests of interest for later replaying latest event
                        match &txlabel {
                            TxLabel::Lock => {
                                self.syncer_state.lock_tx_confs = Some(request.clone());
                            }
                            TxLabel::Cancel => {
                                self.syncer_state.cancel_tx_confs = Some(request.clone());
                            }
                            _ => {}
                        }

                        match txlabel {
                            TxLabel::Funding => {}
                            TxLabel::Lock
                                if self.state.a_refundsig()
                                    && !self.state.a_xmr_locked()
                                    && !self.state.a_buy_published()
                                    && self.remote_params.is_some()
                                    && !self.syncer_state.acc_lock_watched() =>
                            {
                                if let (
                                    Some(Params::Alice(alice_params)),
                                    Some(Params::Bob(bob_params)),
                                ) = (&self.local_params, &self.remote_params)
                                {
                                    let (spend, view) =
                                        aggregate_xmr_spend_view(alice_params, bob_params);
                                    let viewpair = monero::ViewPair { spend, view };
                                    let address = monero::Address::from_viewpair(
                                        self.syncer_state.network.into(),
                                        &viewpair,
                                    );
                                    info!(
                                        "Alice, please send {} to {}",
                                        self.syncer_state
                                            .monero_amount
                                            .to_string()
                                            .bright_green_bold(),
                                        address.addr(),
                                    );
                                    info!(
                                        "reporting success of {} to {}",
                                        address.to_string(),
                                        self.enquirer.clone().unwrap()
                                    );
                                    self.report_success_to(
                                        senders,
                                        self.enquirer.clone(),
                                        Some(address.to_string()),
                                    )?;
                                    let watch_addr_task = self.syncer_state.watch_addr_xmr(
                                        spend,
                                        view,
                                        TxLabel::AccLock,
                                    );
                                    senders.send_to(
                                        ServiceBus::Ctl,
                                        self.identity(),
                                        self.syncer_state.monero_syncer(),
                                        Request::SyncerTask(watch_addr_task),
                                    )?;
                                } else {
                                    error!("remote_params not set for Bob, state {}", self.state)
                                }
                            }
                            TxLabel::Lock
                                if self.temporal_safety.valid_cancel(*confirmations)
                                    && !self.state.a_buy_published() =>
                            {
                                if let Some((tx_label, cancel_tx)) =
                                    self.txs.remove_entry(&TxLabel::Cancel)
                                {
                                    self.broadcast(cancel_tx, tx_label, senders)?
                                }
                            }
                            TxLabel::Lock
                                if self.temporal_safety.safe_buy(*confirmations)
                                    && self.state.swap_role() == SwapRole::Alice
                                    && self.state.a_refundsig()
                                    && !self.state.a_buy_published() =>
                            {
                                let xmr_locked = self.state.a_xmr_locked();
                                if let Some(buy_tx) = self.txs.remove(&TxLabel::Buy) {
                                    self.broadcast(buy_tx, TxLabel::Buy, senders)?;
                                    self.state = State::Alice(AliceState::RefundSigA(RefundSigA {
                                        buy_published: true,
                                        xmr_locked,
                                        // local_params,
                                    }));
                                } else {
                                    warn!(
                                        "Alice doesn't have the buy tx, probably didnt receive \
                                             the BuySig yet: {}",
                                        self.state
                                    );
                                }
                            }
                            TxLabel::Cancel
                                if self.temporal_safety.valid_punish(*confirmations)
                                    && self.state.a_refundsig()
                                    && self.state.a_xmr_locked() =>
                            {
                                trace!("Alice publishes punish tx");
                                if let Some((tx_label, punish_tx)) =
                                    self.txs.remove_entry(&TxLabel::Punish)
                                {
                                    self.broadcast(punish_tx, tx_label, senders)?
                                }
                            }
                            TxLabel::Cancel
                                if self.temporal_safety.safe_refund(*confirmations)
                                    && (self.state.b_buy_sig() || self.state.b_core_arb()) =>
                            {
                                trace!("here Bob publishes refund tx");
                                if let Some((tx_label, refund_tx)) =
                                    self.txs.remove_entry(&TxLabel::Refund)
                                {
                                    self.broadcast(refund_tx, tx_label, senders)?;
                                }
                            }
                            TxLabel::Buy
                                if self.temporal_safety.final_tx(*confirmations, Coin::Bitcoin)
                                    && self.state.a_refundsig()
                                    && self.state.a_buy_published() =>
                            {
                                // FIXME: swap ends here for alice
                                // wallet + farcaster
                                self.state_update(senders, State::Alice(AliceState::FinishA))?;
                                let abort_all = Task::Abort(Abort {
                                    task_target: TaskTarget::AllTasks,
                                });
                                senders.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    self.syncer_state.monero_syncer(),
                                    Request::SyncerTask(abort_all.clone()),
                                )?;
                                senders.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    self.syncer_state.bitcoin_syncer(),
                                    Request::SyncerTask(abort_all),
                                )?;
                                let swap_success_req = Request::SwapSuccess(true);
                                self.send_wallet(
                                    ServiceBus::Ctl,
                                    senders,
                                    swap_success_req.clone(),
                                )?;
                                self.send_ctl(senders, ServiceId::Farcasterd, swap_success_req)?;
                                self.txs.remove(&TxLabel::Cancel);
                                self.txs.remove(&TxLabel::Punish);
                            }
                            tx_label => warn!("tx label {} not supported", tx_label),
                        }
                    }
                    Event::TransactionConfirmations(TransactionConfirmations {
                        id,
                        block: _,
                        confirmations,
                    }) => {
                        self.syncer_state.handle_tx_confs(id, confirmations);
                    }
                    Event::TransactionBroadcasted(event) => {
                        debug!("{}", event)
                    }
                    Event::TaskAborted(event) => {
                        info!("{}", event)
                    }
                    Event::SweepSuccess(event) => {
                        debug!("{}", event)
                    }
                }
            }
            Request::Protocol(Msg::CoreArbitratingSetup(core_arb_setup)) if self.state.reveal() => {
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
                    let txid = tx.clone().extract_tx().txid();
                    let task = self.syncer_state.watch_tx_btc(txid, tx_label);
                    senders.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        self.syncer_state.bitcoin_syncer(),
                        Request::SyncerTask(task),
                    )?;
                }
                trace!("sending peer CoreArbitratingSetup msg: {}", &core_arb_setup);
                self.send_peer(senders, Msg::CoreArbitratingSetup(core_arb_setup.clone()))?;
                let next_state = State::Bob(BobState::CorearbB(core_arb_setup));
                self.state_update(senders, next_state)?;
            }

            Request::Tx(Tx::Lock(btc_lock)) if self.state.b_core_arb() => {
                info!("received {}", TxLabel::Lock);
                self.broadcast(btc_lock, TxLabel::Lock, senders)?;
                if let (Some(Params::Bob(bob_params)), Some(Params::Alice(alice_params))) =
                    (&self.local_params, &self.remote_params)
                {
                    let (spend, view) = aggregate_xmr_spend_view(alice_params, bob_params);
                    let task = self
                        .syncer_state
                        .watch_addr_xmr(spend, view, TxLabel::AccLock);
                    senders.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        self.syncer_state.monero_syncer(),
                        Request::SyncerTask(task),
                    )?
                } else {
                    error!("remote_params not set, state {}", self.state)
                }
            }
            Request::Tx(transaction) => {
                // update state
                match transaction.clone() {
                    Tx::Cancel(tx) => {
                        log_tx_received(TxLabel::Cancel);
                        self.txs.insert(TxLabel::Cancel, tx);
                    }
                    Tx::Refund(tx) => {
                        log_tx_received(TxLabel::Refund);
                        self.txs.insert(TxLabel::Refund, tx);
                    }
                    Tx::Punish(tx) => {
                        log_tx_received(TxLabel::Punish);
                        self.txs.insert(TxLabel::Punish, tx);
                    }
                    Tx::Buy(tx) => {
                        log_tx_received(TxLabel::Buy);
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
                            self.handle_rpc_ctl(senders, source, lock_tx_confs_req)?;
                        }
                    }
                    Tx::Refund(_) | Tx::Punish(_) => {
                        if let Some(cancel_tx_confs_req) = self.syncer_state.cancel_tx_confs.clone()
                        {
                            self.handle_rpc_ctl(senders, source, cancel_tx_confs_req)?;
                        }
                    }
                    _ => {}
                }
            }

            Request::Protocol(Msg::RefundProcedureSignatures(refund_proc_sigs))
                if self.state.reveal() =>
            {
                self.send_peer(senders, Msg::RefundProcedureSignatures(refund_proc_sigs))?;
                trace!("sent peer RefundProcedureSignatures msg");
                let next_state = State::Alice(AliceState::RefundSigA(RefundSigA {
                    xmr_locked: false,
                    buy_published: false,
                }));
                self.state_update(senders, next_state)?;
            }

            Request::Protocol(Msg::BuyProcedureSignature(ref buy_proc_sig))
                if self.state.b_core_arb() =>
            {
                debug!("subscribing with syncer for receiving raw buy tx ");

                let buy_tx = buy_proc_sig.buy.clone().extract_tx();
                let txid = buy_tx.txid();
                // register Buy tx task
                let task = self.syncer_state.watch_tx_btc(txid, TxLabel::Buy);
                senders.send_to(
                    ServiceBus::Ctl,
                    self.identity(),
                    self.syncer_state.bitcoin_syncer(),
                    Request::SyncerTask(task),
                )?;

                let script_pubkey = if buy_tx.output.len() == 1 {
                    buy_tx.output[0].script_pubkey.clone()
                } else {
                    error!("more than one output");
                    return Ok(());
                };
                debug!("subscribe Buy address task");
                let task = self
                    .syncer_state
                    .watch_addr_btc(script_pubkey, TxLabel::Buy);
                senders.send_to(
                    ServiceBus::Ctl,
                    self.identity(),
                    self.syncer_state.bitcoin_syncer(),
                    Request::SyncerTask(task),
                )?;

                let pending_request = PendingRequest {
                    request,
                    dest: self.peer_service.clone(),
                    bus_id: ServiceBus::Msg,
                };
                if self
                    .pending_requests
                    .insert(self.syncer_state.monero_syncer(), vec![pending_request])
                    .is_none()
                {
                    debug!("deferring BuyProcedureSignature msg");
                } else {
                    error!("removed a pending request by mistake")
                };
            }

            Request::GetInfo => {
                fn bmap<T>(remote_peer: &Option<NodeAddr>, v: &T) -> BTreeMap<NodeAddr, T>
                where
                    T: Clone,
                {
                    remote_peer
                        .as_ref()
                        .map(|p| bmap! { p.clone() => v.clone() })
                        .unwrap_or_default()
                }

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
                    // params: self.params, // FIXME
                    // serde::Serialize/Deserialize missing
                    local_keys: dumb!(),
                    remote_keys: bmap(&self.maker_peer, &dumb!()),
                };
                self.send_ctl(senders, source, Request::SwapInfo(info))?;
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
        senders: &mut Senders,
        params: Params,
    ) -> Result<request::Commit, Error> {
        let msg = format!(
            "{} {} with id {:#}",
            "Proposing to the Maker".bright_white_bold(),
            "that I take the swap offer".bright_white_bold(),
            self.swap_id().bright_blue_italic()
        );
        info!("{}", &msg);
        let engine = CommitmentEngine;
        let commitment = match params {
            Params::Bob(params) => request::Commit::BobParameters(
                CommitBobParameters::commit_to_bundle(self.swap_id(), &engine, params),
            ),
            Params::Alice(params) => request::Commit::AliceParameters(
                CommitAliceParameters::commit_to_bundle(self.swap_id(), &engine, params),
            ),
        };
        // Ignoring possible reporting errors here and after: do not want to
        // halt the swap just because the client disconnected
        let enquirer = self.enquirer.clone();
        let _ = self.report_progress_to(senders, &enquirer, msg);

        Ok(commitment)
    }

    pub fn maker_commit(
        &mut self,
        senders: &mut Senders,
        peerd: &ServiceId,
        swap_id: SwapId,
        params: &Params,
    ) -> Result<request::Commit, Error> {
        let msg = format!(
            "{} as Maker with swap id {:#} from Taker remote peer {}",
            "Accepting swap".bright_white_bold(),
            swap_id.bright_blue_italic(),
            peerd.bright_blue_italic()
        );
        info!("{}", msg);

        // Ignoring possible reporting errors here and after: do not want to
        // halt the channel just because the client disconnected
        let enquirer = self.enquirer.clone();
        let _ = self.report_progress_to(senders, &enquirer, msg);

        let engine = CommitmentEngine;
        let commitment = match params.clone() {
            Params::Bob(params) => request::Commit::BobParameters(
                CommitBobParameters::commit_to_bundle(self.swap_id(), &engine, params),
            ),
            Params::Alice(params) => request::Commit::AliceParameters(
                CommitAliceParameters::commit_to_bundle(self.swap_id(), &engine, params),
            ),
        };

        let msg = format!(
            "{} swap {:#} from remote peer Taker {}",
            "Making".bright_green_bold(),
            swap_id.bright_green_italic(),
            peerd.bright_green_italic()
        );
        info!("{}", msg);
        let _ = self.report_success_to(senders, &enquirer, Some(msg));
        // self.send_peer(senders, ProtocolMessages::Commit(swap_req.clone()))?;
        Ok(commitment)
    }
}

pub fn get_swap_id(source: &ServiceId) -> Result<SwapId, Error> {
    if let ServiceId::Swap(swap_id) = source {
        Ok(*swap_id)
    } else {
        Err(Error::Farcaster("Not swapd".to_string()))
    }
}

fn log_tx_seen(txlabel: &TxLabel, txid: &Txid) {
    info!(
        "{} tx in mempool or blockchain, sending it to wallet: {}",
        txlabel,
        txid.addr(),
    );
}

fn log_tx_received(txlabel: TxLabel) {
    info!("received tx {} from {}", txlabel, ServiceId::Wallet);
}

fn aggregate_xmr_spend_view(
    alice_params: &AliceParameters<BtcXmr>,
    bob_params: &BobParameters<BtcXmr>,
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

#[derive(Debug)]
struct PendingRequest {
    request: Request,
    dest: ServiceId,
    bus_id: ServiceBus,
}
