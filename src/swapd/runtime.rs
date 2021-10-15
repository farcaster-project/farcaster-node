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

use crate::syncerd::{opts::Coin, Abort, HeightChanged, WatchHeight, XmrAddressAddendum};
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
use monero::cryptonote::hash::keccak_256;
use request::{Commit, InitSwap, Params, Reveal, TakeCommit, Tx};

pub fn run(
    config: Config,
    swap_id: SwapId,
    chain: Chain,
    public_offer: PublicOffer<BtcXmr>,
    local_trade_role: TradeRole,
) -> Result<(), Error> {
    let Offer {
        cancel_timelock,
        punish_timelock,
        maker_role, // SwapRole of maker (Alice or Bob)
        ..
    } = public_offer.offer.clone();

    // alice or bob
    let local_swap_role = match local_trade_role {
        TradeRole::Maker => maker_role,
        TradeRole::Taker => maker_role.other(),
    };

    let init_state = match local_swap_role {
        SwapRole::Alice => State::Alice(AliceState::StartA(local_trade_role, public_offer)),
        SwapRole::Bob => State::Bob(BobState::StartB(local_trade_role, public_offer)),
    };

    info!("Initial state: {}", init_state.bright_white_bold());

    let temporal_safety = TemporalSafety {
        cancel_timelock: cancel_timelock.as_u32(),
        punish_timelock: punish_timelock.as_u32(),
        btc_finality_thr: 0,
        race_thr: 3,
        xmr_finality_thr: 0,
    };

    temporal_safety.valid_params()?;
    let syncer_state = SyncerState {
        tx_tasks: none!(),
        monero_height: 0,
        bitcoin_height: 0,
        task_counter: 0,
        confirmation_bound: 50000,
        lock_tx_confs: None,
        cancel_tx_confs: None,
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
    pending_requests: HashMap<ServiceId, Vec<PendingRequest>>,
    txs: HashMap<TxLabel, bitcoin::Transaction>,
    #[allow(dead_code)]
    storage: Box<dyn storage::Driver>,
    remote_params: Option<Params>,
}

struct TemporalSafety {
    cancel_timelock: BlockHeight,
    punish_timelock: BlockHeight,
    race_thr: BlockHeight,
    btc_finality_thr: BlockHeight,
    xmr_finality_thr: BlockHeight,
}

type BlockHeight = u32;

impl TemporalSafety {
    /// check if temporal params are in correct order
    fn valid_params(&self) -> Result<(), Error> {
        let btc_finality = self.btc_finality_thr;
        let xmr_finality = self.xmr_finality_thr;
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

struct SyncerState {
    bitcoin_height: u64,
    monero_height: u64,
    tx_tasks: HashMap<u32, TxLabel>,
    task_counter: u32,
    confirmation_bound: u32,
    lock_tx_confs: Option<Request>,
    cancel_tx_confs: Option<Request>,
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
    RevealA(Option<Params>, Commit), // local, remote
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
    RevealB(Option<Params>, Commit), // local, remote
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

impl State {
    fn swap_role(&self) -> SwapRole {
        match self {
            State::Alice(_) => SwapRole::Alice,
            State::Bob(_) => SwapRole::Bob,
        }
    }
    fn xmr_locked(&self) -> bool {
        if let State::Alice(AliceState::RefundSigA(RefundSigA { xmr_locked, .. })) = self {
            *xmr_locked
        } else {
            false
        }
    }
    fn buy_published(&self) -> bool {
        if let State::Alice(AliceState::RefundSigA(RefundSigA { buy_published, .. })) = self {
            *buy_published
        } else {
            false
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
    fn from_height(&self, coin: Coin) -> u64 {
        let lookback = match coin {
            Coin::Monero => 300,
            Coin::Bitcoin => 50,
        }; // blocks
        let height = self.height(coin);
        if height > lookback {
            height - lookback
        } else {
            height
        }
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
        if &new_height > &height {
            info!("{:?} new height {}", coin, &new_height);
            *height = new_height;
        } else {
            warn!("block height did not increment, maybe syncer sends multiple events");
        }
    }

    fn new_taskid(&mut self) -> u32 {
        self.task_counter += 1;
        self.task_counter
    }

    fn watch_addr_btc(&mut self, script_pubkey: Script, id: u32) -> Task {
        let from_height = self.from_height(Coin::Bitcoin);
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
        accordant_shared_keys: Vec<TaggedElement<SharedKeyId, monero::PrivateKey>>,
        swap_role: SwapRole,
    ) -> Result<Task, Error> {
        let view_key = *accordant_shared_keys
            .into_iter()
            .find(|vk| vk.tag() == &SharedKeyId::new(SHARED_VIEW_KEY_ID))
            .unwrap()
            .elem();

        if swap_role == SwapRole::Alice {
            let keypair = monero::ViewPair {
                spend,
                view: view_key.clone(),
            };
            let address = monero::Address::from_viewpair(monero::Network::Stagenet, &keypair);
            info!("Alice, please send xmr to {}", address.addr());
        }

        let from_height = self.from_height(Coin::Monero);

        let addendum = AddressAddendum::Monero(XmrAddressAddendum {
            spend_key: spend.to_bytes(),
            view_key: view.to_bytes(),
            from_height,
        });

        let id = self.new_taskid();
        let watch_addr = WatchAddress {
            id,
            lifetime: self.task_lifetime(Coin::Monero),
            addendum,
            include_tx: Boolean::True,
        };
        Ok(Task::WatchAddress(watch_addr))
    }
    fn acc_lock_watched(&self) -> bool {
        self.tx_tasks
            .values()
            .find(|&&x| x == TxLabel::AccLock)
            .is_some()
    }
    fn handle_tx_confs(&self, id: &u32, confirmations: &Option<u32>) {
        if let Some(txlabel) = self.tx_tasks.get(id) {
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

    fn broadcast(
        &mut self,
        tx: bitcoin::Transaction,
        tx_label: TxLabel,
        senders: &mut Senders,
    ) -> Result<(), Error> {
        let req = Request::SyncerTask(Task::BroadcastTransaction(BroadcastTransaction {
            id: self.syncer_state.new_taskid(),
            tx: bitcoin::consensus::serialize(&tx),
        }));

        info!("Broadcasting {} tx {}", tx_label, tx.txid().addr());
        Ok(senders.send_to(
            ServiceBus::Ctl,
            self.identity(),
            ServiceId::Syncer(Coin::Bitcoin),
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
            Err(Error::Farcaster(format!(
                "{}: expected {}, found {}",
                "Incorrect peer connection".to_string(),
                self.peer_service,
                source
            )))?
        }
        let msg_bus = ServiceBus::Msg;
        match &request {
            Request::Protocol(msg) => {
                if msg.swap_id() != self.swap_id() {
                    Err(Error::Farcaster(format!(
                        "{}: expected {}, found {}",
                        "Incorrect swap_id ".to_string(),
                        self.swap_id(),
                        msg.swap_id(),
                    )))?
                }
                match &msg {
                    // we are taker and the maker committed, now we reveal after checking
                    // whether we're Bob or Alice and that we're on a compatible state
                    Msg::MakerCommit(remote_commit) => {
                        trace!("received commitment from counterparty, can now reveal");
                        let next_state = match self.state.clone() {
                            State::Alice(AliceState::CommitA(CommitC { local_params, .. })) => {
                                Ok(State::Alice(AliceState::RevealA(
                                    Some(local_params),
                                    remote_commit.clone(),
                                )))
                            }
                            State::Bob(BobState::CommitB(
                                CommitC {
                                    local_params,
                                    remote_commit: None,
                                    ..
                                },
                                addr,
                            )) => {
                                let id = self.syncer_state.new_taskid();
                                let task =
                                    self.syncer_state.watch_addr_btc(addr.script_pubkey(), id);
                                if self
                                    .syncer_state
                                    .tx_tasks
                                    .insert(id, TxLabel::Funding)
                                    .is_none()
                                {
                                    info!("Bob subscribes for Funding address");
                                    self.send_ctl(
                                        senders,
                                        ServiceId::Syncer(Coin::Bitcoin),
                                        Request::SyncerTask(task),
                                    )?;
                                    Ok(State::Bob(BobState::RevealB(
                                        Some(local_params),
                                        remote_commit.clone(),
                                    )))
                                } else {
                                    Err(Error::Farcaster(s!("tx already registered with that id")))
                                }
                            }
                            state => Err(Error::Farcaster(format!(
                                "Must be on Commit state, found {}",
                                state
                            ))),
                        }?;

                        if let State::Alice(AliceState::CommitA(CommitC {
                            trade_role: TradeRole::Taker,
                            ..
                        }))
                        | State::Bob(BobState::CommitB(
                            CommitC {
                                trade_role: TradeRole::Taker,
                                ..
                            },
                            _,
                        )) = &self.state
                        {
                            trace!("Watch height bitcoin");
                            let watch_height_bitcoin = Task::WatchHeight(WatchHeight {
                                id: self.syncer_state.new_taskid(),
                                lifetime: self.syncer_state.task_lifetime(Coin::Bitcoin),
                            });
                            senders.send_to(
                                ServiceBus::Ctl,
                                self.identity(),
                                ServiceId::Syncer(Coin::Bitcoin),
                                Request::SyncerTask(watch_height_bitcoin),
                            )?;

                            trace!("Watch height monero");
                            let watch_height_monero = Task::WatchHeight(WatchHeight {
                                id: self.syncer_state.new_taskid(),
                                lifetime: self.syncer_state.task_lifetime(Coin::Monero),
                            });
                            senders.send_to(
                                ServiceBus::Ctl,
                                self.identity(),
                                ServiceId::Syncer(Coin::Monero),
                                Request::SyncerTask(watch_height_monero),
                            )?;
                        }

                        self.send_wallet(msg_bus, senders, request)?;
                        info!("State transition: {}", next_state.bright_white_bold());
                        self.state = next_state;
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
                                    Some(local_params),
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
                                let id = self.syncer_state.new_taskid();
                                let watch_addr_task =
                                    self.syncer_state.watch_addr_btc(addr.script_pubkey(), id);
                                if self
                                    .syncer_state
                                    .tx_tasks
                                    .insert(id, TxLabel::Funding)
                                    .is_none()
                                {
                                    info!("Bob subscribes for Funding address");
                                    self.send_ctl(
                                        senders,
                                        ServiceId::Syncer(Coin::Bitcoin),
                                        Request::SyncerTask(watch_addr_task),
                                    )?;
                                    Ok((
                                        State::Bob(BobState::RevealB(
                                            Some(local_params),
                                            remote_commit.clone(),
                                        )),
                                        remote_commit,
                                    ))
                                } else {
                                    Err(Error::Farcaster(s!(
                                        "there is already a tx registered with that id"
                                    )))
                                }
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
                                    Err(Error::Farcaster(err_msg.to_string()))?
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
                                    Err(Error::Farcaster(err_msg.to_string()))?
                                }
                            },
                            Reveal::Proof(_) => {
                                error!("this should have been caught by another pattern!");
                                None
                            }
                        };
                        if remote_params_candidate.is_some() {
                            self.remote_params = remote_params_candidate
                        }
                        info!("{:?} sets remote_params", self.state.swap_role());

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
                                // when receiving from wallet

                                trace!(
                                    "This pending request will be called later: {:?}",
                                    &pending_request
                                );
                                let pending_requests = self.pending_requests.get_mut(&ServiceId::Wallet).expect("should already have received Reveal::Proof, so this key should exist.");
                                if pending_requests.len() != 1 {
                                    error!("should have a single pending Reveal::Proof only FIXME")
                                }
                                pending_requests.push(pending_request);
                            }
                        }
                        // up to here for both maker and taker, following only Maker

                        // if did not yet reveal, maker only. on the msg flow as
                        // of 2021-07-13 taker reveals first
                        match &self.state {
                            State::Alice(AliceState::CommitA(CommitC {
                                trade_role: TradeRole::Maker,
                                local_params,
                                ..
                            }))
                            | State::Bob(BobState::CommitB(
                                CommitC {
                                    trade_role: TradeRole::Maker,
                                    local_params,
                                    ..
                                },
                                _,
                            )) => {
                                trace!("Watch height bitcoin");
                                let watch_height_bitcoin = Task::WatchHeight(WatchHeight {
                                    id: self.syncer_state.new_taskid(),
                                    lifetime: self.syncer_state.task_lifetime(Coin::Bitcoin),
                                });
                                senders.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    ServiceId::Syncer(Coin::Bitcoin),
                                    Request::SyncerTask(watch_height_bitcoin),
                                )?;

                                trace!("Watch height monero");
                                let watch_height_monero = Task::WatchHeight(WatchHeight {
                                    id: self.syncer_state.new_taskid(),
                                    lifetime: self.syncer_state.task_lifetime(Coin::Monero),
                                });
                                senders.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    ServiceId::Syncer(Coin::Monero),
                                    Request::SyncerTask(watch_height_monero),
                                )?;

                                info!("State transition: {}", next_state.bright_white_bold());
                                self.state = next_state;
                            }
                            _ => debug!(
                                "You are the Taker, which revealed already, nothing to reveal."
                            ),
                        }
                    }
                    // alice receives, bob sends
                    Msg::CoreArbitratingSetup(CoreArbitratingSetup {
                        swap_id,
                        lock,
                        cancel,
                        refund,
                        cancel_sig,
                    }) => {
                        if swap_id != &self.swap_id() {
                            error!("Swapd not responsible for swap {}", swap_id);
                            return Ok(());
                        }
                        if let State::Alice(AliceState::RevealA(_, _)) = self.state {
                            for (&tx, tx_label) in [lock, cancel, refund].iter().zip([
                                TxLabel::Lock,
                                TxLabel::Cancel,
                                TxLabel::Refund,
                            ]) {
                                let txid = tx.clone().extract_tx().txid();
                                let id = self.syncer_state.new_taskid();
                                self.syncer_state.tx_tasks.insert(id, tx_label.clone());
                                info!(
                                    "Alice watches tx {} {}",
                                    tx_label.bright_green_bold(),
                                    txid.addr()
                                );
                                let task = Task::WatchTransaction(WatchTransaction {
                                    id,
                                    lifetime: self.syncer_state.task_lifetime(Coin::Bitcoin),
                                    hash: txid.to_vec(),
                                    confirmation_bound: self.syncer_state.confirmation_bound,
                                });
                                senders.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    ServiceId::Syncer(Coin::Bitcoin),
                                    Request::SyncerTask(task),
                                )?;
                            }
                            // senders.send_to(ServiceBus::Ctl, self.identity(),
                            // ServiceId::Syncer(Coin::Bitcoin), req)?;
                            self.send_wallet(msg_bus, senders, request)?;
                        } else {
                            error!(
                                "Wrong state: Only Alice receives CoreArbitratingSetup msg \
                                 through peer connection at state RevealA"
                            )
                        }
                    }
                    // bob receives, alice sends
                    Msg::RefundProcedureSignatures(_) => {
                        if let State::Bob(BobState::CorearbB(_)) = self.state {
                            self.send_wallet(msg_bus, senders, request)?;
                        } else {
                            error!(
                                "Wrong state: Bob receives RefundProcedureSignatures msg \
                                 through peer connection in state CorearbB"
                            )
                        }
                    }
                    // alice receives, bob sends
                    Msg::BuyProcedureSignature(BuyProcedureSignature { buy, .. }) => {
                        // Alice verifies that she has sent refund procedure signatures before
                        // processing the buy signatures from Bob
                        if let State::Alice(AliceState::RefundSigA(..)) = self.state {
                            let tx = buy.clone().extract_tx();

                            let txid = tx.txid();
                            let id = self.syncer_state.new_taskid();
                            self.syncer_state.tx_tasks.insert(id, TxLabel::Buy);
                            // notify the syncer to watch for the buy transaction
                            let task = Task::WatchTransaction(WatchTransaction {
                                id,
                                lifetime: self.syncer_state.task_lifetime(Coin::Bitcoin),
                                hash: txid.to_vec(),
                                confirmation_bound: self.syncer_state.confirmation_bound,
                            });
                            senders.send_to(
                                ServiceBus::Ctl,
                                self.identity(),
                                ServiceId::Syncer(Coin::Bitcoin),
                                Request::SyncerTask(task),
                            )?;
                            self.send_wallet(msg_bus, senders, request)?
                        } else {
                            error!("Wrong state: must be RefundProcedureSignatures");
                            return Ok(());
                        }
                    }

                    // bob and alice
                    Msg::Abort(_) => Err(Error::Farcaster("Abort not yet supported".to_string()))?,
                    Msg::Ping(_) | Msg::Pong(_) | Msg::PingPeer => {
                        unreachable!("ping/pong must remain in peerd, and unreachable in swapd")
                    }
                }
            }
            // let _ = self.report_progress_to(senders, &enquirer, msg);
            // let _ = self.report_success_to(senders, &enquirer, Some(msg));
            _ => {
                error!("MSG RPC can be only used for forwarding FWP messages");
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
            (Request::Hello, ServiceId::Syncer(_)) => {
                info!("Source: {} is connected", source);
            }

            (Request::Hello, _) => {
                info!("Source: {} is connected", source);
            }
            (
                _,
                ServiceId::Farcasterd
                | ServiceId::Wallet
                | ServiceId::Syncer(Coin::Bitcoin)
                | ServiceId::Syncer(Coin::Monero),
            ) => {}
            _ => Err(Error::Farcaster(
                "Permission Error: only Farcasterd, Wallet and Syncer can can control swapd"
                    .to_string(),
            ))?,
        };

        let ctl_bus = ServiceBus::Ctl;
        match request {
            Request::TakeSwap(InitSwap {
                peerd,
                report_to,
                local_params,
                swap_id,
                remote_commit: None,
                funding_address, // Some(_) for Bob, None for Alice
            }) => {
                if &ServiceId::Swap(swap_id) != &self.identity {
                    error!(
                        "{}: {}",
                        "This swapd instance is not reponsible for swap_id", swap_id
                    );
                    return Ok(());
                };
                self.peer_service = peerd.clone();
                self.enquirer = report_to.clone();

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
                let (next_state, public_offer) = match (self.state.clone(), funding_address) {
                    (State::Bob(BobState::StartB(local_trade_role, public_offer)), Some(addr)) => {
                        Ok((
                            (State::Bob(BobState::CommitB(
                                CommitC {
                                    trade_role: local_trade_role,
                                    local_params: local_params.clone(),
                                    local_commit: local_commit.clone(),
                                    remote_commit: None,
                                },
                                addr,
                            ))),
                            public_offer,
                        ))
                    }
                    (State::Alice(AliceState::StartA(local_trade_role, public_offer)), None) => {
                        Ok((
                            (State::Alice(AliceState::CommitA(CommitC {
                                trade_role: local_trade_role,
                                local_params: local_params.clone(),
                                local_commit: local_commit.clone(),
                                remote_commit: None,
                            }))),
                            public_offer,
                        ))
                    }
                    _ => Err(Error::Farcaster(s!(
                        "Wrong state: Expects Start state, and funding_address"
                    ))),
                }?;
                let public_offer_hex = public_offer.to_hex();
                let take_swap = TakeCommit {
                    commit: local_commit,
                    public_offer_hex,
                    swap_id,
                };
                self.send_peer(senders, Msg::TakerCommit(take_swap))?;
                info!("State transition: {}", next_state.bright_white_bold());
                self.state = next_state;
            }
            Request::Protocol(Msg::Reveal(reveal)) => match self.state.clone() {
                State::Alice(AliceState::RevealA(Some(local_params), remote_commit))
                | State::Bob(BobState::RevealB(Some(local_params), remote_commit)) => {
                    let reveal_proof = Msg::Reveal(reveal);
                    let swap_id = reveal_proof.swap_id();
                    self.send_peer(senders, reveal_proof)?;
                    info!("forwarded reveal_proof");
                    let reveal_params: Reveal = (swap_id, local_params.clone()).into();
                    self.send_peer(senders, Msg::Reveal(reveal_params))?;
                    info!("sent reveal_proof to peerd");
                    let next_state = match self.state {
                        State::Alice(_) => State::Alice(AliceState::RevealA(None, remote_commit)),
                        State::Bob(_) => State::Bob(BobState::RevealB(None, remote_commit)),
                    };
                    info!("State transition: {}", next_state.bright_white_bold());
                    self.state = next_state;
                }
                _ => {
                    error!("Wrong state: Expects RevealA | RevealB");
                }
            },

            Request::MakeSwap(InitSwap {
                peerd,
                report_to,
                local_params,
                swap_id,
                remote_commit: Some(remote_commit),
                funding_address, // Some(_) for Bob, None for Alice
            }) => {
                self.peer_service = peerd.clone();
                if let ServiceId::Peer(ref addr) = peerd {
                    self.maker_peer = Some(addr.clone());
                }

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

                let next_state = match (&self.state, funding_address) {
                    (State::Bob(BobState::StartB(trade_role, _)), Some(addr)) => {
                        Ok(State::Bob(BobState::CommitB(
                            CommitC {
                                trade_role: *trade_role,
                                local_params: local_params.clone(),
                                local_commit: local_commit.clone(),
                                remote_commit: Some(remote_commit.clone()),
                            },
                            addr,
                        )))
                    }
                    (State::Alice(AliceState::StartA(trade_role, _)), None) => {
                        Ok(State::Alice(AliceState::CommitA(CommitC {
                            trade_role: *trade_role,
                            local_params: local_params.clone(),
                            local_commit: local_commit.clone(),
                            remote_commit: Some(remote_commit.clone()),
                        })))
                    }
                    _ => Err(Error::Farcaster(s!("Wrong state: Expects Start"))),
                }?;

                trace!("sending peer MakerCommit msg {}", &local_commit);
                self.send_peer(senders, Msg::MakerCommit(local_commit))?;

                info!("State transition: {}", next_state.bright_white_bold());
                trace!("setting commit_remote and commit_local msg");
                self.state = next_state;
            }
            Request::FundingUpdated => {
                if source != ServiceId::Wallet {
                    error!("Only wallet permited");
                    return Ok(());
                }
                trace!("funding updated received from wallet");
                let mut pending_requests = self
                    .pending_requests
                    .remove(&source)
                    .expect("should have pending Reveal{Proof} requests");
                if pending_requests.len() == 2 {
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
                    if let State::Bob(BobState::RevealB(..)) = self.state {
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
                            senders.send_to(
                                bus_id_proof,
                                self.identity(),
                                dest_proof,
                                request_proof,
                            )?
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
                    } else {
                        error!("Expected state RevealB, found {}", self.state);
                    }
                } else {
                    error!("pending requests not found")
                }
            }
            // Request::SyncerEvent(ref event) => match (&event, source) {
            // handle monero events here
            // }
            Request::SyncerEvent(ref event) if &source == &ServiceId::Syncer(Coin::Monero) => {
                match &event {
                    Event::HeightChanged(HeightChanged { height, .. }) => {
                        self.syncer_state
                            .handle_height_change(*height, Coin::Monero);
                        info!("monero new height {}", height.bright_green_italic())
                    }
                    Event::AddressTransaction(AddressTransaction {
                        id,
                        hash,
                        amount,
                        block,
                        tx,
                    }) => {
                        let id = self.syncer_state.new_taskid();
                        if let State::Alice(AliceState::RefundSigA(RefundSigA {
                            xmr_locked, ..
                        })) = &mut self.state
                        {
                            if !*xmr_locked {
                                *xmr_locked = true;
                            } else {
                                warn!("xmr_locked was already set to true")
                            }
                        }
                        info!(
                            "Watching {} {}",
                            TxLabel::AccLock.bright_green_bold(),
                            hex::encode(&hash).addr()
                        );
                        self.syncer_state.tx_tasks.insert(id, TxLabel::AccLock);
                        let watch_tx = Task::WatchTransaction(WatchTransaction {
                            id,
                            lifetime: self.syncer_state.task_lifetime(Coin::Monero),
                            hash: hash.clone(),
                            confirmation_bound: self.syncer_state.confirmation_bound,
                        });
                        senders.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            ServiceId::Syncer(Coin::Monero),
                            Request::SyncerTask(watch_tx),
                        )?;
                    }
                    Event::TransactionConfirmations(TransactionConfirmations {
                        id,
                        block,
                        confirmations: Some(confirmations),
                    }) if self.temporal_safety.final_tx(*confirmations, Coin::Monero)
                        && self.state.swap_role() == SwapRole::Bob
                        && self.pending_requests.get(&source).is_some() =>
                    {
                        // error!("not checking tx rcvd is accordant lock");
                        // TODO: Check length of pending_requests == 1
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
                            let next_state = match self.state {
                                State::Bob(BobState::CorearbB(..)) => {
                                    Ok(State::Bob(BobState::BuySigB))
                                }
                                _ => Err(Error::Farcaster(s!("Wrong state: must be CorearbB "))),
                            }?;
                            info!("sending buyproceduresignature at state {}", &self.state);
                            senders.send_to(bus_id, self.identity(), dest, request)?;
                            info!("State transition: {}", next_state.bright_white_bold());
                            self.state = next_state;
                        } else {
                            error!(
                                "Not buyproceduresignatures {} or not Msg bus found {}",
                                request, bus_id
                            );
                        }
                    }
                    Event::TransactionConfirmations(TransactionConfirmations {
                        id,
                        block,
                        confirmations,
                    }) if self.syncer_state.tx_tasks.get(id).is_some() => {
                        self.syncer_state.handle_tx_confs(id, confirmations);
                    }
                    Event::AddressTransaction(AddressTransaction {
                        id,
                        hash,
                        amount,
                        block,
                        tx,
                    }) => {
                        let id = self.syncer_state.new_taskid();

                        let watch_tx = Task::WatchTransaction(WatchTransaction {
                            id,
                            lifetime: self.syncer_state.task_lifetime(Coin::Monero),
                            hash: hash.clone(),
                            confirmation_bound: self.syncer_state.confirmation_bound,
                        });
                        senders.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            ServiceId::Syncer(Coin::Monero),
                            Request::SyncerTask(watch_tx),
                        )?;
                    }
                    Event::TransactionConfirmations(TransactionConfirmations {
                        id,
                        block,
                        confirmations: Some(confirmations),
                    }) if confirmations > &self.temporal_safety.xmr_finality_thr
                        && self.state.swap_role() == SwapRole::Bob
                        && self.pending_requests.get(&source).is_some() =>
                    {
                        error!("not checking tx rcvd is accordant lock");
                        // TODO: Check length of pending_requests == 1
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
                            let next_state = match self.state {
                                State::Bob(BobState::CorearbB(..)) => {
                                    Ok(State::Bob(BobState::BuySigB))
                                }
                                _ => Err(Error::Farcaster(s!("Wrong state: must be CorearbB "))),
                            }?;
                            info!("sending buyproceduresignature at state {}", &self.state);
                            senders.send_to(bus_id, self.identity(), dest, request)?;
                            info!("State transition: {}", next_state.bright_white_bold());
                            self.state = next_state;
                        } else {
                            error!(
                                "Not buyproceduresignatures {} or not Msg bus found {}",
                                request, bus_id
                            );
                        }
                    }
                    Event::TaskAborted(_) => {}
                    event => {
                        error!("event not handled {}", event)
                    }
                }
            }
            Request::SyncerEvent(ref event) if &source == &ServiceId::Syncer(Coin::Bitcoin) => {
                match &event {
                    Event::HeightChanged(HeightChanged { height, .. }) => {
                        self.syncer_state
                            .handle_height_change(*height, Coin::Bitcoin);
                        info!("bitcoin new height {}", height.bright_green_italic())
                    }
                    Event::AddressTransaction(AddressTransaction {
                        id,
                        hash,
                        amount,
                        block,
                        tx,
                    }) => {
                        let tx = bitcoin::Transaction::deserialize(tx)?;
                        trace!(
                            "Received AddressTransaction, processing tx {}",
                            tx.txid().addr()
                        );
                        if let Some(txlabel) = self.syncer_state.tx_tasks.get(id) {
                            match txlabel {
                                TxLabel::Funding => {
                                    info!(
                                        "Funding tx in mempool or blockchain, \
                                     sending it to wallet: {}",
                                        &tx.txid().addr()
                                    );
                                    let req = Request::Tx(Tx::Funding(tx));
                                    self.send_wallet(ServiceBus::Ctl, senders, req)?;
                                }
                                TxLabel::Buy => {
                                    if let State::Bob(BobState::BuySigB) = self.state {
                                        info!(
                                            "{} tx in mempool or blockchain, \
                                           sending it to wallet: {}",
                                            &TxLabel::Buy,
                                            &tx.txid().addr(),
                                        );
                                        let req = Request::Tx(Tx::Buy(tx));
                                        self.send_wallet(ServiceBus::Ctl, senders, req)?
                                    } else {
                                        error!(
                                            "expected BuySigB, found {}, maybe you reused the \
                                         external address? txid {}",
                                            self.state,
                                            tx.txid().addr(),
                                        )
                                    }
                                }
                                TxLabel::Refund => {
                                    if let State::Alice(AliceState::RefundSigA(RefundSigA {
                                        xmr_locked: true,
                                        buy_published: false,
                                    })) = self.state
                                    {
                                        info!(
                                            "found refund tx in mempool or blockchain, \
                                             sending it to wallet: {}",
                                            &tx.txid().addr()
                                        );
                                        let req = Request::Tx(Tx::Refund(tx));
                                        self.send_wallet(ServiceBus::Ctl, senders, req)?
                                    } else {
                                        error!("expected RefundSigA(true), found {}", self.state);
                                    }
                                }
                                txlabel => {
                                    error!(
                                        "address transaction event not supported for tx {}",
                                        txlabel
                                    )
                                }
                            }
                        } else {
                            error!(
                            "Transaction event received, unknow address with task id {} and txid {:?}",
                            id.addr(), Txid::from_slice(hash).unwrap().addr()
                        );
                        }
                    }
                    Event::TransactionConfirmations(TransactionConfirmations {
                        id,
                        block,
                        confirmations: Some(confirmations),
                    }) if self.temporal_safety.final_tx(*confirmations, Coin::Bitcoin) => {
                        self.syncer_state.handle_tx_confs(id, &Some(*confirmations));
                        if let Some(txlabel) = self.syncer_state.tx_tasks.get(&id) {
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
                                    if !self.state.xmr_locked()
                                        && self.remote_params.is_some()
                                        && !self.syncer_state.acc_lock_watched() =>
                                {
                                    if let State::Alice(AliceState::RefundSigA(RefundSigA {
                                        buy_published: false,
                                        xmr_locked: false,
                                    })) = self.state
                                    {
                                        if let Some(Params::Bob(BobParameters {
                                            spend,
                                            accordant_shared_keys,
                                            ..
                                        })) = self.remote_params.clone()
                                        {
                                            error!("here alice watches accordant lock address, broadcast manually");
                                            info!(
                                                "Alice subscribes for monero address with syncer"
                                            );
                                            let watch_addr_task =
                                                self.syncer_state.watch_addr_xmr(
                                                    spend,
                                                    accordant_shared_keys,
                                                    self.state.swap_role(),
                                                )?;
                                            senders.send_to(
                                                ServiceBus::Ctl,
                                                self.identity(),
                                                ServiceId::Syncer(Coin::Monero),
                                                Request::SyncerTask(watch_addr_task),
                                            )?;
                                        } else {
                                            error!(
                                                "remote_params not set for Bob, state {}",
                                                self.state
                                            )
                                        }
                                    }
                                }
                                TxLabel::Lock
                                    if self.temporal_safety.valid_cancel(*confirmations) =>
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
                                        && !self.state.buy_published() =>
                                {
                                    if let State::Alice(AliceState::RefundSigA(RefundSigA {
                                        buy_published: false,
                                        xmr_locked,
                                    })) = self.state
                                    {
                                        if let Some(buy_tx) = self.txs.remove(&TxLabel::Buy) {
                                            self.broadcast(buy_tx, TxLabel::Buy, senders)?;
                                            self.state =
                                                State::Alice(AliceState::RefundSigA(RefundSigA {
                                                    buy_published: true,
                                                    xmr_locked,
                                                }));
                                        } else {
                                            warn!(
                                            "Alice doesn't have the buy tx, probably didnt receive \
                                             the BuySig yet: {}",
                                            self.state
                                        );
                                        }
                                    } else {
                                        error!(
                                        "wrong state: expected RefundProcedureSignatures, found {}",
                                        &self.state
                                    )
                                    }
                                }
                                TxLabel::Lock if self.state.swap_role() == SwapRole::Alice => {
                                    error!("make state strict");
                                    if let State::Alice(..) = self.state {
                                        error!("here alice watchs accordant lock address, and broadcast accordant lock");
                                        if let Some(Params::Alice(AliceParameters {
                                            spend,
                                            accordant_shared_keys,
                                            ..
                                        })) = self.remote_params.clone()
                                        {
                                            info!(
                                                "Alice subscribes for monero address with syncer"
                                            );
                                            let watch_addr_task =
                                                self.syncer_state.watch_addr_xmr(
                                                    spend,
                                                    accordant_shared_keys,
                                                    self.state.swap_role(),
                                                )?;
                                            senders.send_to(
                                                ServiceBus::Ctl,
                                                self.identity(),
                                                ServiceId::Syncer(Coin::Monero),
                                                Request::SyncerTask(watch_addr_task),
                                            )?;
                                            error!("xmr lock transaction not available, get it from wallet, commented out code");
                                        }
                                    }
                                }
                                TxLabel::Cancel
                                    if self.temporal_safety.valid_punish(*confirmations) =>
                                {
                                    trace!("Alice publishes punish tx");
                                    if let Some((tx_label, punish_tx)) =
                                        self.txs.remove_entry(&TxLabel::Punish)
                                    {
                                        if let State::Alice(AliceState::RefundSigA(RefundSigA {
                                            xmr_locked: true,
                                            ..
                                        })) = self.state
                                        {
                                            info!(
                                                "Broadcasting btc punish {}",
                                                punish_tx.txid().addr()
                                            );
                                            self.broadcast(punish_tx, tx_label, senders)?
                                        }
                                    }
                                }
                                TxLabel::Cancel
                                    if self.temporal_safety.safe_refund(*confirmations) =>
                                {
                                    if let State::Bob(BobState::BuySigB)
                                    | State::Bob(BobState::CorearbB(..)) = self.state
                                    {
                                        trace!("here Bob publishes refund tx");
                                        if let Some((tx_label, refund_tx)) =
                                            self.txs.remove_entry(&TxLabel::Refund)
                                        {
                                            self.broadcast(refund_tx, tx_label, senders)?;
                                        }
                                    }
                                }
                                TxLabel::Buy => {
                                    info!("found buy by txid")
                                }
                                tx_label => warn!("tx label {} not supported", tx_label),
                            }
                        } else {
                            warn!(
                                "received event with unknown transaction and task id {}",
                                &id
                            )
                        }
                    }
                    Event::TransactionConfirmations(TransactionConfirmations {
                        id,
                        block,
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
                }
            }
            Request::Protocol(Msg::CoreArbitratingSetup(core_arb_setup)) => {
                let next_state = match self.state {
                    State::Bob(BobState::RevealB(_, _)) => {
                        // below tx is unsigned
                        Ok(State::Bob(BobState::CorearbB(core_arb_setup.clone())))
                    }
                    _ => Err(Error::Farcaster(s!("Wrong state: must be RevealB"))),
                }?;
                let CoreArbitratingSetup {
                    swap_id,
                    lock,
                    cancel,
                    refund,
                    cancel_sig,
                } = core_arb_setup.clone();
                for (tx, tx_label) in [lock, cancel, refund].iter().zip([
                    TxLabel::Lock,
                    TxLabel::Cancel,
                    TxLabel::Refund,
                ]) {
                    let txid = tx.clone().extract_tx().txid();
                    let id = self.syncer_state.new_taskid();
                    self.syncer_state.tx_tasks.insert(id, tx_label);
                    info!(
                        "Bob registers tx {} with syncer {}",
                        tx_label.addr(),
                        txid.addr()
                    );
                    let task = Task::WatchTransaction(WatchTransaction {
                        id,
                        lifetime: self.syncer_state.task_lifetime(Coin::Bitcoin),
                        hash: txid.to_vec(),
                        confirmation_bound: self.syncer_state.confirmation_bound,
                    });
                    senders.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        ServiceId::Syncer(Coin::Bitcoin),
                        Request::SyncerTask(task),
                    )?;
                }
                trace!("sending peer CoreArbitratingSetup msg: {}", &core_arb_setup);
                self.send_peer(senders, Msg::CoreArbitratingSetup(core_arb_setup))?;
                info!("State transition: {}", next_state.bright_white_bold());
                self.state = next_state;
            }

            Request::Tx(Tx::Lock(btc_lock)) => {
                if let State::Bob(BobState::CorearbB(..)) = self.state {
                    self.broadcast(btc_lock, TxLabel::Lock, senders)?;
                    if let Some(Params::Alice(AliceParameters {
                        spend,
                        accordant_shared_keys,
                        ..
                    })) = self.remote_params.clone()
                    {
                        info!("Bob subscribes for monero address with syncer");
                        let task = self.syncer_state.watch_addr_xmr(
                            spend,
                            accordant_shared_keys,
                            self.state.swap_role(),
                        )?;
                        senders.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            ServiceId::Syncer(Coin::Monero),
                            Request::SyncerTask(task),
                        )?
                    } else {
                        error!("remote_params not set, state {}", self.state)
                    }
                } else {
                    error!("Wrong state: must be CorearbB, found {}", &self.state)
                }
            }
            Request::Tx(transaction) => {
                // update state
                match transaction.clone() {
                    Tx::Cancel(tx) => {
                        info!("received cancel");
                        self.txs.insert(TxLabel::Cancel, tx);
                    }
                    Tx::Refund(tx) => {
                        info!("received refund");
                        self.txs.insert(TxLabel::Refund, tx);
                    }
                    Tx::Punish(tx) => {
                        info!("received punish");
                        self.txs.insert(TxLabel::Punish, tx);
                    }
                    Tx::Buy(tx) => {
                        info!("received buy");
                        self.txs.insert(TxLabel::Buy, tx);
                    }
                    Tx::Funding(_) => unreachable!("not handled in swapd"),
                    Tx::Lock(_) => unreachable!("handled above"),
                }
                // replay last tx confirmation event received from syncer, recursing
                let source = ServiceId::Syncer(Coin::Bitcoin);
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

            Request::Protocol(Msg::RefundProcedureSignatures(refund_proc_sigs)) => {
                let next_state = match self.state {
                    State::Alice(AliceState::RevealA(_, _)) => {
                        Ok(State::Alice(AliceState::RefundSigA(RefundSigA {
                            xmr_locked: false,
                            buy_published: false,
                        })))
                    }
                    _ => Err(Error::Farcaster(s!("Wrong state: must be RevealA"))),
                }?;
                debug!("sending peer RefundProcedureSignatures msg");
                self.send_peer(senders, Msg::RefundProcedureSignatures(refund_proc_sigs))?;
                info!("State transition: {}", next_state.bright_white_bold());
                self.state = next_state;
            }

            Request::Protocol(Msg::BuyProcedureSignature(ref buy_proc_sig)) => {
                if let State::Bob(BobState::CorearbB(..)) = self.state {
                    debug!("subscribing with syncer for receiving raw buy tx ");

                    let buy_tx = buy_proc_sig.buy.clone().extract_tx();
                    let txid = buy_tx.txid();
                    // register Buy tx task
                    let id_tx = self.syncer_state.new_taskid();
                    self.syncer_state.tx_tasks.insert(id_tx, TxLabel::Buy);
                    info!("subscribing with syncer for receiving buy tx updates");
                    let task = Task::WatchTransaction(WatchTransaction {
                        id: id_tx,
                        lifetime: self.syncer_state.task_lifetime(Coin::Bitcoin),
                        hash: txid.to_vec(),
                        confirmation_bound: self.syncer_state.confirmation_bound,
                    });
                    senders.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        ServiceId::Syncer(Coin::Bitcoin),
                        Request::SyncerTask(task),
                    )?;

                    let script_pubkey = if buy_tx.output.len() == 1 {
                        buy_tx.output[0].script_pubkey.clone()
                    } else {
                        error!("more than one output");
                        return Ok(());
                    };
                    let id_addr = self.syncer_state.new_taskid();
                    self.syncer_state.tx_tasks.insert(id_addr, TxLabel::Buy);
                    debug!("subscribe Buy address task");
                    let task = self.syncer_state.watch_addr_btc(script_pubkey, id_addr);
                    senders.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        ServiceId::Syncer(Coin::Bitcoin),
                        Request::SyncerTask(task),
                    )?;

                    let pending_request = PendingRequest {
                        request,
                        dest: self.peer_service.clone(),
                        bus_id: ServiceBus::Msg,
                    };
                    if self
                        .pending_requests
                        .insert(ServiceId::Syncer(Coin::Monero), vec![pending_request])
                        .is_none()
                    {
                        debug!("deferring BuyProcedureSignature msg");
                    } else {
                        error!("removed a pending request by mistake")
                    };
                // self.send_peer(senders,
                // Msg::BuyProcedureSignature(buy_proc_sig))?;
                } else {
                    error!("Wrong state: must be CorearbB ");
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
                        .unwrap_or(Duration::from_secs(0)),
                    since: self
                        .started
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap_or(Duration::from_secs(0))
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
        let commitment = match params.clone() {
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
        let _ = self.report_progress_to(senders, &enquirer, msg)?;

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
        Ok(commitment.clone())
    }
}

pub fn get_swap_id(source: ServiceId) -> Result<SwapId, Error> {
    if let ServiceId::Swap(swap_id) = source {
        Ok(swap_id)
    } else {
        Err(Error::Farcaster("Not swapd".to_string()))
    }
}

#[derive(Debug)]
struct PendingRequest {
    request: Request,
    dest: ServiceId,
    bus_id: ServiceBus,
}
