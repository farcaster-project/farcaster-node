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

use crate::syncerd::{opts::Coin, Abort, HeightChanged, WatchHeight};
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
    consensus::{self, Encodable as FarEncodable},
    monero::Monero,
    negotiation::{Offer, PublicOffer},
    protocol_message::{
        BuyProcedureSignature, CommitAliceParameters, CommitBobParameters, CoreArbitratingSetup,
        RefundProcedureSignatures,
    },
    role::{Arbitrating, SwapRole, TradeRole},
    swap::btcxmr::{BtcXmr, KeyManager},
    swap::SwapId,
    transaction::{Broadcastable, Transaction, TxLabel, Witnessable},
};
use internet2::zmqsocket::{self, ZmqSocketAddr, ZmqType};
use internet2::{
    session, CreateUnmarshaller, NodeAddr, Session, TypedEnum, Unmarshall, Unmarshaller,
};
use lnpbp::{chain::AssetId, Chain};
use microservices::esb::{self, Handler};
use monero::cryptonote::hash::keccak_256;
use request::{Commit, Datum, InitSwap, Params, Reveal, TakeCommit};

pub fn run(
    config: Config,
    swap_id: SwapId,
    chain: Chain,
    public_offer: PublicOffer<BtcXmr>,
    local_trade_role: TradeRole,
) -> Result<(), Error> {
    let Offer {
        network,
        arbitrating_blockchain,
        accordant_blockchain,
        arbitrating_amount,
        accordant_amount,
        cancel_timelock,
        punish_timelock,
        fee_strategy,
        maker_role, // SwapRole of maker (Alice or Bob)
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

    let temporal_safety = TemporalSafety {
        cancel_timelock,
        punish_timelock,
        tx_finality_thr: 0,
        confirmation_bound: 50000,
        race_thr: 1,
    };

    temporal_safety.valid_params()?;
    let syncer_state = SyncerState {
        task_lifetime: u64::MAX,
        txs_status: none!(),
        block_height: 0,
        task_counter: 0,
    };

    let runtime = Runtime {
        identity: ServiceId::Swap(swap_id),
        peer_service: ServiceId::Loopback,
        chain,
        state: init_state,
        maker_peer: None,
        started: SystemTime::now(),
        accordant_amount,
        arbitrating_amount,
        fee_strategy,
        accordant_blockchain,
        arbitrating_blockchain,
        network,
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
    };
    let broker = false;
    Service::run(config, runtime, broker)
}

// FIXME: State enum should carry over the data that is accumulated over time,
// and corresponding lines should be removed from Runtime
pub struct Runtime {
    identity: ServiceId,
    peer_service: ServiceId,
    chain: Chain,
    state: State,
    // remote_state: State,
    maker_peer: Option<NodeAddr>,
    started: SystemTime,
    accordant_amount: monero::Amount,
    arbitrating_amount: bitcoin::Amount,
    fee_strategy: FeeStrategy<SatPerVByte>,
    network: blockchain::Network,
    arbitrating_blockchain: Bitcoin<SegwitV0>,
    accordant_blockchain: Monero,
    enquirer: Option<ServiceId>,
    syncer_state: SyncerState,
    temporal_safety: TemporalSafety,
    pending_requests: HashMap<ServiceId, PendingRequest>,
    #[allow(dead_code)]
    storage: Box<dyn storage::Driver>,
}

struct TemporalSafety {
    cancel_timelock: CSVTimelock,
    punish_timelock: CSVTimelock,
    race_thr: u32,
    tx_finality_thr: u32,
    confirmation_bound: u16,
}

impl TemporalSafety {
    /// check if temporal params are in correct order
    fn valid_params(&self) -> Result<(), Error> {
        let finality = self.tx_finality_thr;
        let cancel = self.cancel_timelock.as_u32();
        let punish = self.punish_timelock.as_u32();
        let race = self.race_thr;
        if finality < cancel && cancel < punish && finality < race && punish > race {
            Ok(())
        } else {
            Err(Error::Farcaster(s!(
                "unsafe and invalid temporal parameters, timelocks, race and tx finality params"
            )))
        }
    }
    fn final_tx(&self, confs: u32) -> bool {
        confs >= self.tx_finality_thr
    }
    fn valid_cancel(&self, lock_confirmations: u32) -> bool {
        // lock must be final, valid after lock_minedblock + cancel_timelock
        self.final_tx(lock_confirmations) && lock_confirmations >= self.cancel_timelock.as_u32()
    }
    fn safe_refund(&self, cancel_confirmations: u32) -> bool {
        // cancel must be final, but refund shall not be raced with punish
        self.final_tx(cancel_confirmations)
            && cancel_confirmations < (self.punish_timelock.as_u32() - self.race_thr)
    }
    fn valid_punish(&self, cancel_confirmations: u32) -> bool {
        self.final_tx(cancel_confirmations) && cancel_confirmations >= self.punish_timelock.as_u32()
    }
}

struct SyncerState {
    task_lifetime: u64,
    txs_status: HashMap<u32, (TxLabel, TxStatus)>,
    block_height: u64,
    task_counter: u32,
}

#[derive(Display, Clone)]
pub enum AliceState {
    #[display("Start")]
    StartA(TradeRole, PublicOffer<BtcXmr>), // local, both
    #[display("Commit")]
    CommitA(TradeRole, Params, Commit, Option<Commit>), // local, local, local, remote
    #[display("Reveal")]
    RevealA(Commit), // remote
    #[display("RefundProcSigs")]
    RefundSigA(bool), // monero locked
    #[display("Finish")]
    FinishA,
}

#[derive(Display, Clone)]
pub enum BobState {
    #[display("Start")]
    StartB(TradeRole, PublicOffer<BtcXmr>), // local, both
    #[display("Commit")]
    CommitB(TradeRole, Params, Commit, Option<Commit>, bitcoin::Address), /* local, local,
                                                                           * local, remote,
                                                                           * local */
    #[display("Reveal")]
    RevealB(Commit), // remote
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
    fn handle_height_change(&mut self, block_height: u64) {
        if &block_height > &self.block_height {
            self.block_height = block_height;
            self.task_lifetime = block_height + 500;
        } else {
            warn!("block height did not increment, maybe syncer sends multiple events");
        }
    }

    fn new_taskid(&mut self) -> u32 {
        self.task_counter += 1;
        self.task_counter
    }

    fn watch_addr(&mut self, script_pubkey: Script, id: u32) -> Request {
        let lookback = 100; // blocks
        let from_height = if self.block_height > lookback {
            self.block_height - lookback
        } else {
            self.block_height
        };
        let addendum = BtcAddressAddendum {
            address: None,
            from_height,
            script_pubkey,
        };
        Request::SyncerTask(Task::WatchAddress(WatchAddress {
            id,
            lifetime: self.task_lifetime,
            addendum: AddressAddendum::Bitcoin(addendum),
            include_tx: Boolean::True,
        }))
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
                        let (next_state, local_params) = match self.state.clone() {
                            State::Alice(AliceState::CommitA(_, local_params, _, None)) => Ok((
                                State::Alice(AliceState::RevealA(remote_commit.clone())),
                                local_params,
                            )),
                            State::Bob(BobState::CommitB(_, local_params, _, None, addr)) => {
                                let id = self.syncer_state.new_taskid();
                                let watch_addr_req =
                                    self.syncer_state.watch_addr(addr.script_pubkey(), id);
                                if self
                                    .syncer_state
                                    .txs_status
                                    .insert(id, (TxLabel::Funding, TxStatus::Notfinal))
                                    .is_none()
                                {
                                    info!("Bob subscribes for Funding address");
                                    self.send_ctl(
                                        senders,
                                        ServiceId::Syncer(Coin::Bitcoin),
                                        watch_addr_req,
                                    )?;
                                    Ok((
                                        State::Bob(BobState::RevealB(remote_commit.clone())),
                                        local_params,
                                    ))
                                } else {
                                    Err(Error::Farcaster(s!("tx already registered with that id")))
                                }
                            }
                            state => Err(Error::Farcaster(format!(
                                "Must be on Commit state, found {}",
                                state
                            ))),
                        }?;

                        if let State::Alice(AliceState::CommitA(TradeRole::Taker, ..))
                        | State::Bob(BobState::CommitB(TradeRole::Taker, ..)) = &self.state
                        {
                            trace!("Watch height");
                            let watch_height = Task::WatchHeight(WatchHeight {
                                id: self.syncer_state.new_taskid(),
                                lifetime: self.syncer_state.task_lifetime,
                            });
                            senders.send_to(
                                ServiceBus::Ctl,
                                self.identity(),
                                ServiceId::Syncer(Coin::Bitcoin),
                                Request::SyncerTask(watch_height),
                            )?;
                        }

                        let reveal: Reveal = (msg.swap_id(), local_params.clone()).into();
                        self.send_wallet(msg_bus, senders, request)?;
                        self.send_peer(senders, Msg::Reveal(reveal))?;
                        info!("State transition: {}", next_state.bright_blue_bold());
                        self.state = next_state;
                    }
                    Msg::TakerCommit(_) => {
                        unreachable!(
                            "msg handled by farcasterd/walletd, and indirectly here by \
                             Ctl Request::MakeSwap"
                        )
                    }
                    // bob and alice
                    // store parameters from counterparty if we have not received them yet.
                    // if we're maker, also reveal to taker if their commitment is valid.
                    Msg::Reveal(reveal) => {
                        let (next_state, remote_commit) = match self.state.clone() {
                            // counterparty has already revealed commitment, i.e. we're
                            // maker and counterparty is taker. now proceed to reveal state.
                            State::Alice(AliceState::CommitA(.., Some(remote_commit))) => Ok((
                                State::Alice(AliceState::RevealA(remote_commit.clone())),
                                remote_commit,
                            )),
                            State::Bob(BobState::CommitB(.., Some(remote_commit), addr)) => {
                                let id = self.syncer_state.new_taskid();
                                let watch_addr_req =
                                    self.syncer_state.watch_addr(addr.script_pubkey(), id);
                                if self
                                    .syncer_state
                                    .txs_status
                                    .insert(id, (TxLabel::Funding, TxStatus::Notfinal))
                                    .is_none()
                                {
                                    info!("Bob subscribes for Funding address");
                                    self.send_ctl(
                                        senders,
                                        ServiceId::Syncer(Coin::Bitcoin),
                                        watch_addr_req,
                                    )?;
                                    Ok((
                                        State::Bob(BobState::RevealB(remote_commit.clone())),
                                        remote_commit,
                                    ))
                                } else {
                                    Err(Error::Farcaster(s!(
                                        "there is already a tx registered iwth that id"
                                    )))
                                }
                            }
                            // we're already in reveal state, i.e. we're taker, so don't change
                            // state once counterparty reveals too.
                            State::Alice(AliceState::RevealA(remote_commit)) => Ok((
                                State::Alice(AliceState::RevealA(remote_commit.clone())),
                                remote_commit,
                            )),
                            State::Bob(BobState::RevealB(remote_commit)) => Ok((
                                State::Bob(BobState::RevealB(remote_commit.clone())),
                                remote_commit,
                            )),
                            state => Err(Error::Farcaster(format!(
                                "Must be on Commit or Reveal state, found {}",
                                state
                            ))),
                        }?;

                        // parameter processing irrespective of maker & taker role
                        let core_wallet = KeyManager::new_keyless();
                        let remote_params = match reveal {
                            Reveal::Alice(reveal) => match &remote_commit {
                                Commit::Alice(commit) => {
                                    commit.verify_with_reveal(&core_wallet, reveal.clone())?;
                                    Params::Alice(reveal.clone().into())
                                }
                                _ => {
                                    let err_msg = "expected Some(Commit::Alice(commit))";
                                    error!("{}", err_msg);
                                    Err(Error::Farcaster(err_msg.to_string()))?
                                }
                            },
                            Reveal::Bob(reveal) => match &remote_commit {
                                Commit::Bob(commit) => {
                                    commit.verify_with_reveal(&core_wallet, reveal.clone())?;
                                    Params::Bob(reveal.clone().into())
                                }
                                _ => {
                                    let err_msg = "expected Some(Commit::Bob(commit))";
                                    error!("{}", err_msg);
                                    Err(Error::Farcaster(err_msg.to_string()))?
                                }
                            },
                        };
                        // pass request on to wallet daemon so that it can set remote params
                        match self.state {
                            // validaded state above, no need to check again
                            State::Alice(..) => self.send_wallet(msg_bus, senders, request)?,
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
                                if self
                                    .pending_requests
                                    .insert(ServiceId::Wallet, pending_request)
                                    .is_none()
                                {
                                } else {
                                    error!("A pending request was removed, FIXME")
                                }
                            }
                        }
                        // up to here for both maker and taker, following only Maker

                        // if did not yet reveal, maker only. on the msg flow as
                        // of 2021-07-13 taker reveals first
                        let id = self.syncer_state.new_taskid();
                        match &self.state {
                            State::Alice(AliceState::CommitA(
                                TradeRole::Maker,
                                local_params,
                                ..,
                            ))
                            | State::Bob(BobState::CommitB(TradeRole::Maker, local_params, ..)) => {
                                trace!("Watch height");
                                let watch_height = Task::WatchHeight(WatchHeight {
                                    id,
                                    lifetime: self.syncer_state.task_lifetime,
                                });

                                senders.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    ServiceId::Syncer(Coin::Bitcoin),
                                    Request::SyncerTask(watch_height),
                                )?;

                                trace!("received commitment from counterparty, can now reveal");
                                let reveal: Reveal = (self.swap_id(), local_params.clone()).into();
                                self.send_peer(senders, Msg::Reveal(reveal))?;
                                info!("State transition: {}", next_state.bright_blue_bold());
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
                        if let State::Alice(AliceState::RevealA(_)) = self.state {
                            // FIXME subscribe syncer to Accordant + arbitrating locks and buy +
                            for (&tx, tx_label) in [lock, cancel, refund].iter().zip([
                                TxLabel::Lock,
                                TxLabel::Cancel,
                                TxLabel::Refund,
                            ]) {
                                let txid = tx.clone().extract_tx().txid();
                                let id = self.syncer_state.new_taskid();
                                if self
                                    .syncer_state
                                    .txs_status
                                    .insert(id, (tx_label.clone(), TxStatus::Notfinal))
                                    .is_none()
                                {
                                    info!(
                                        "Alice registers tx {} with syncer {}",
                                        tx_label.addr(),
                                        txid.addr()
                                    );
                                    let task = Task::WatchTransaction(WatchTransaction {
                                        id,
                                        lifetime: self.syncer_state.task_lifetime,
                                        hash: txid.to_vec(),
                                        confirmation_bound: self.temporal_safety.confirmation_bound,
                                    });
                                    senders.send_to(
                                        ServiceBus::Ctl,
                                        self.identity(),
                                        ServiceId::Syncer(Coin::Bitcoin),
                                        Request::SyncerTask(task),
                                    )?;
                                } else {
                                    error!("task id already registered");
                                    return Ok(());
                                }
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
                    Msg::RefundProcedureSignatures(RefundProcedureSignatures {
                        swap_id,
                        cancel_sig,
                        refund_adaptor_sig,
                    }) => {
                        if let State::Bob(BobState::CorearbB(_)) = self.state {
                            // FIXME subscribe syncer to Accordant + arbitrating locks and buy +
                            // cancel txs
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
                            if self
                                .syncer_state
                                .txs_status
                                .insert(id, (TxLabel::Buy, TxStatus::Notfinal))
                                .is_none()
                            {
                                // notify the syncer to watch for the buy transaction
                                let task = Task::WatchTransaction(WatchTransaction {
                                    id,
                                    lifetime: self.syncer_state.task_lifetime,
                                    hash: txid.to_vec(),
                                    confirmation_bound: self.temporal_safety.confirmation_bound,
                                });
                                senders.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    ServiceId::Syncer(Coin::Bitcoin),
                                    Request::SyncerTask(task),
                                )?;
                                self.send_wallet(msg_bus, senders, request)?
                            }
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
            (Request::Hello, ServiceId::Syncer(Coin::Bitcoin)) => {
                info!("Source: {} is connected", source);
            }

            (Request::Hello, _) => {
                info!("Source: {} is connected", source);
            }
            (_, ServiceId::Farcasterd | ServiceId::Wallet | ServiceId::Syncer(Coin::Bitcoin)) => {}
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
                                local_trade_role,
                                local_params.clone(),
                                local_commit.clone(),
                                None,
                                addr,
                            ))),
                            public_offer,
                        ))
                    }
                    (State::Alice(AliceState::StartA(local_trade_role, public_offer)), None) => {
                        Ok((
                            (State::Alice(AliceState::CommitA(
                                local_trade_role,
                                local_params.clone(),
                                local_commit.clone(),
                                None,
                            ))),
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
                info!("State transition: {}", next_state.bright_blue_bold());
                self.state = next_state;
            }

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
                            *trade_role,
                            local_params.clone(),
                            local_commit.clone(),
                            Some(remote_commit.clone()),
                            addr,
                        )))
                    }
                    (State::Alice(AliceState::StartA(trade_role, _)), None) => {
                        Ok(State::Alice(AliceState::CommitA(
                            *trade_role,
                            local_params.clone(),
                            local_commit.clone(),
                            Some(remote_commit.clone()),
                        )))
                    }
                    _ => Err(Error::Farcaster(s!("Wrong state: Expects Start"))),
                }?;

                trace!("sending peer MakerCommit msg {}", &local_commit);
                self.send_peer(senders, Msg::MakerCommit(local_commit))?;

                info!("State transition: {}", next_state.bright_blue_bold());
                trace!("setting commit_remote and commit_local msg");
                self.state = next_state;
            }
            Request::FundingUpdated => {
                if source != ServiceId::Wallet {
                    error!("Only wallet permited");
                    return Ok(());
                }
                trace!("funding updated received from wallet");
                if let Some(PendingRequest {
                    request,
                    dest,
                    bus_id,
                }) = self.pending_requests.remove(&source)
                {
                    // FIXME state management
                    // if let State::Alice(AliceState::Re) =self.state {}
                    if let Request::Protocol(Msg::Reveal(Reveal::Alice(_))) = &request {
                        trace!(
                            "sending request {} to {} on bus {}",
                            &request,
                            &dest,
                            &bus_id
                        );
                        senders.send_to(bus_id, self.identity(), dest, request)?
                    } else {
                        error!("Not the expected request: found {:?}", request);
                    }
                } else {
                    error!("pending request not found")
                }
            }
            // Request::SyncerEvent(ref event) => match (&event, source) {
            // handle monero events here
            // }
            Request::SyncerEvent(ref event) => match &event {
                Event::HeightChanged(HeightChanged { height, .. }) => {
                    self.syncer_state.handle_height_change(*height);
                    info!("height changed {}", height)
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
                    if let Some((txlabel, status)) = self.syncer_state.txs_status.get(id) {
                        match txlabel {
                            TxLabel::Funding => {
                                info!("Funding transaction received, forwarding to wallet");
                                let req = Request::Datum(Datum::Funding(tx));
                                self.send_wallet(ServiceBus::Ctl, senders, req)?;
                                senders.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    ServiceId::Syncer(Coin::Bitcoin),
                                    Request::SyncerTask(Task::Abort(Abort { id: *id })),
                                )?;
                            }
                            TxLabel::Buy => {
                                if let State::Bob(BobState::BuySigB) = self.state {
                                    info!(
                                        "found buy tx in mempool or blockchain, \
                                           sending it to wallet: {}",
                                        &tx.txid().addr()
                                    );
                                    let req = Request::Datum(Datum::FullySignedBuy(tx));
                                    self.send_wallet(ServiceBus::Ctl, senders, req)?
                                } else {
                                    error!("not BuyProcSigB")
                                }
                            }
                            TxLabel::Refund => {
                                if let State::Alice(AliceState::RefundSigA(true)) = self.state {
                                    error!("forward tx to walletd to extract the refund monero key")
                                }
                            }
                            txlabel => {
                                error!("address transaction event not supported for tx {}", txlabel)
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
                    confirmations,
                }) if self.temporal_safety.final_tx(*confirmations as u32) => {
                    if let Some((txlabel, status)) = self.syncer_state.txs_status.get_mut(&id) {
                        *status = TxStatus::Final;
                        info!(
                            "Transaction {} is now final after {} confirmations",
                            txlabel, confirmations
                        );
                        match txlabel {
                            TxLabel::Funding => {}
                            TxLabel::Lock
                                if self.temporal_safety.valid_cancel(*confirmations as u32) =>
                            {
                                error!("shall publish cancel transaction here");
                            }
                            TxLabel::Lock => {
                                if let Some(PendingRequest {
                                    request,
                                    dest,
                                    bus_id,
                                }) = self.pending_requests.remove(&source)
                                {
                                    if let (
                                        Request::Protocol(Msg::BuyProcedureSignature(_)),
                                        ServiceBus::Msg,
                                    ) = (&request, &bus_id)
                                    {
                                        let next_state = match self.state {
                                            State::Bob(BobState::CorearbB(..)) => {
                                                Ok(State::Bob(BobState::BuySigB))
                                            }
                                            _ => Err(Error::Farcaster(s!(
                                                "Wrong state: must be CorearbB "
                                            ))),
                                        }?;
                                        error!("this should be in Accordant lock, not Arbitrating lock finality");
                                        info!(
                                            "sending buyproceduresignature at state {}",
                                            &self.state
                                        );
                                        senders.send_to(bus_id, self.identity(), dest, request)?;
                                        info!(
                                            "State transition: {}",
                                            next_state.bright_blue_bold()
                                        );
                                        self.state = next_state;
                                    } else {
                                        error!(
                                            "Not buyproceduresignatures {} or not (Msg) bus found {}",
                                            request, dest, bus_id
                                        );
                                    }
                                }
                            }
                            TxLabel::Cancel
                                if self.temporal_safety.valid_punish(*confirmations as u32) =>
                            {
                                error!("here Alice publishes punish tx")
                            }
                            TxLabel::Cancel
                                if self.temporal_safety.safe_refund(*confirmations as u32) =>
                            {
                                if let State::Bob(BobState::BuySigB) = self.state {
                                    error!("here Bob publishes refund tx")
                                }
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
                    if let Some((txlabel, status)) = self.syncer_state.txs_status.get_mut(&id) {
                        info!("tx {} has {} confirmations", txlabel, confirmations);
                    } else {
                        error!(
                            "received event with unknown transaction and task id {}",
                            &id
                        )
                    }
                }
                Event::TransactionBroadcasted(event) => {
                    info!("{}", event)
                }
                Event::TaskAborted(event) => {
                    info!("{}", event)
                }
            },
            Request::Protocol(Msg::CoreArbitratingSetup(core_arb_setup)) => {
                let next_state = match self.state {
                    State::Bob(BobState::RevealB(_)) => {
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
                    if self
                        .syncer_state
                        .txs_status
                        .insert(id, (tx_label, TxStatus::Notfinal))
                        .is_none()
                    {
                        info!(
                            "Bob registers tx {} with syncer {}",
                            tx_label.addr(),
                            txid.addr()
                        );
                        let task = Task::WatchTransaction(WatchTransaction {
                            id,
                            lifetime: self.syncer_state.task_lifetime,
                            hash: txid.to_vec(),
                            confirmation_bound: self.temporal_safety.confirmation_bound,
                        });
                        senders.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            ServiceId::Syncer(Coin::Bitcoin),
                            Request::SyncerTask(task),
                        )?;
                    } else {
                        error!("task id already registered");
                        return Ok(());
                    }
                }
                trace!("sending peer CoreArbitratingSetup msg: {}", &core_arb_setup);
                self.send_peer(senders, Msg::CoreArbitratingSetup(core_arb_setup))?;
                info!("State transition: {}", next_state.bright_blue_bold());
                self.state = next_state;
            }

            Request::Datum(Datum::SignedArbitratingLock(btc_lock)) => {
                if let State::Bob(BobState::CorearbB(ref core_arb)) = self.state {
                    let req =
                        Request::SyncerTask(Task::BroadcastTransaction(BroadcastTransaction {
                            id: self.syncer_state.new_taskid(),
                            tx: bitcoin::consensus::serialize(&btc_lock),
                        }));

                    info!("Broadcasting btc lock {}", btc_lock.txid().addr());
                    senders.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        ServiceId::Syncer(Coin::Bitcoin),
                        req,
                    )?;
                } else {
                    error!("Wrong state: must be RevealB, found {}", &self.state)
                }
            }
            Request::Datum(Datum::FullySignedBuy(buy_tx)) => {
                trace!("received fullysigned from wallet");
                if let State::Alice(AliceState::RefundSigA(..)) = self.state {
                    let req =
                        Request::SyncerTask(Task::BroadcastTransaction(BroadcastTransaction {
                            id: self.syncer_state.new_taskid(),
                            tx: bitcoin::consensus::serialize(&buy_tx),
                        }));
                    info!("broadcasting buy tx {}", buy_tx.txid().addr());
                    senders.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        ServiceId::Syncer(Coin::Bitcoin),
                        req,
                    )?;
                } else {
                    error!(
                        "wrong state: expected RefundProcedureSignatures, found {}",
                        &self.state
                    )
                }
            }

            Request::Protocol(Msg::RefundProcedureSignatures(refund_proc_sigs)) => {
                let locked_monero = false;
                let next_state = match self.state {
                    State::Alice(AliceState::RevealA(_)) => {
                        Ok(State::Alice(AliceState::RefundSigA(locked_monero)))
                    }
                    _ => Err(Error::Farcaster(s!("Wrong state: must be RevealA"))),
                }?;
                debug!("sending peer RefundProcedureSignatures msg");
                self.send_peer(senders, Msg::RefundProcedureSignatures(refund_proc_sigs))?;
                info!("State transition: {}", next_state.bright_blue_bold());
                self.state = next_state;
            }

            Request::Protocol(Msg::BuyProcedureSignature(ref buy_proc_sig)) => {
                if let State::Bob(BobState::CorearbB(..)) = self.state {
                    debug!("subscribing with syncer for receiving raw buy tx ");

                    let buy_tx = buy_proc_sig.buy.clone().extract_tx();
                    let txid = buy_tx.txid();
                    // register Buy tx task
                    let id_tx = self.syncer_state.new_taskid();
                    self.syncer_state
                        .txs_status
                        .insert(id_tx, (TxLabel::Buy, TxStatus::Notfinal));
                    info!("subscribing with syncer for receiving buy tx updates");
                    let task = Task::WatchTransaction(WatchTransaction {
                        id: id_tx,
                        lifetime: self.syncer_state.task_lifetime,
                        hash: txid.to_vec(),
                        confirmation_bound: self.temporal_safety.confirmation_bound,
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
                    self.syncer_state
                        .txs_status
                        .insert(id_addr, (TxLabel::Buy, TxStatus::Notfinal));
                    debug!("subscribe Buy address task");
                    let req = self.syncer_state.watch_addr(script_pubkey, id_addr);
                    senders.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        ServiceId::Syncer(Coin::Bitcoin),
                        req,
                    )?;

                    let pending_request = PendingRequest {
                        request,
                        dest: self.peer_service.clone(),
                        bus_id: ServiceBus::Msg,
                    };
                    if self
                        .pending_requests
                        .insert(ServiceId::Syncer(Coin::Bitcoin), pending_request)
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
                error!("Request is not supported by the CTL interface");
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
            "Proposing to the Maker".bright_blue_bold(),
            "that I take the swap offer".bright_blue_bold(),
            self.swap_id().bright_blue_italic()
        );
        info!("{}", &msg);
        let core_wallet = KeyManager::new_keyless();
        let commitment = match params.clone() {
            Params::Bob(params) => request::Commit::Bob(CommitBobParameters::commit_to_bundle(
                self.swap_id(),
                &core_wallet,
                params,
            )),
            Params::Alice(params) => request::Commit::Alice(
                CommitAliceParameters::commit_to_bundle(self.swap_id(), &core_wallet, params),
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
            "Accepting swap".bright_blue_bold(),
            swap_id.bright_blue_italic(),
            peerd.bright_blue_italic()
        );
        info!("{}", msg);

        // Ignoring possible reporting errors here and after: do not want to
        // halt the channel just because the client disconnected
        let enquirer = self.enquirer.clone();
        let _ = self.report_progress_to(senders, &enquirer, msg);

        let core_wallet = KeyManager::new_keyless();
        let commitment = match params.clone() {
            Params::Bob(params) => request::Commit::Bob(CommitBobParameters::commit_to_bundle(
                self.swap_id(),
                &core_wallet,
                params,
            )),
            Params::Alice(params) => request::Commit::Alice(
                CommitAliceParameters::commit_to_bundle(self.swap_id(), &core_wallet, params),
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

enum TxStatus {
    Final,
    Notfinal,
}

enum AddressOrScript {
    Address(bitcoin::Address),
    Script(bitcoin::Script),
}

#[derive(Debug)]
struct PendingRequest {
    request: Request,
    dest: ServiceId,
    bus_id: ServiceBus,
}
