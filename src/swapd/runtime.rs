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

use std::{
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
use bitcoin::util::{bip143::SigHashCache, psbt::PartiallySignedTransaction};
use bitcoin::{consensus::Encodable, secp256k1};
use bitcoin::{
    hashes::{hex::FromHex, sha256, Hash, HashEngine},
    Txid,
};
use bitcoin::{OutPoint, SigHashType, Transaction};

use farcaster_core::{
    bitcoin::{
        fee::SatPerVByte, segwitv0::SegwitV0, tasks::BtcAddressAddendum, timelock::CSVTimelock,
        Bitcoin,
    },
    blockchain::{self, FeeStrategy},
    consensus::{self, Encodable as FarEncodable},
    monero::Monero,
    negotiation::{Offer, PublicOffer},
    protocol_message::{
        BuyProcedureSignature, CommitAliceParameters, CommitBobParameters, CoreArbitratingSetup,
    },
    role::{Arbitrating, SwapRole, TradeRole},
    swap::btcxmr::{BtcXmr, KeyManager as CoreWallet},
    swap::SwapId,
    syncer::{
        AddressTransaction, Boolean, BroadcastTransaction, Event, Task, TransactionConfirmations,
        WatchAddress, WatchTransaction,
    },
    transaction::TxLabel,
};
use internet2::zmqsocket::{self, ZmqSocketAddr, ZmqType};
use internet2::{
    session, CreateUnmarshaller, NodeAddr, Session, TypedEnum, Unmarshall, Unmarshaller,
};
use lnp::payment::bolt3::{ScriptGenerators, TxGenerators};
use lnp::payment::htlc::{HtlcKnown, HtlcSecret};
use lnp::payment::{self, AssetsBalance, Lifecycle};
use lnp::{message, Messages, TempChannelId as TempSwapId};
use lnpbp::{chain::AssetId, Chain};
use microservices::esb::{self, Handler};
use monero::cryptonote::hash::keccak_256;
use request::{Commit, InitSwap, Params, Reveal, TakeCommit};

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

    let runtime = Runtime {
        identity: ServiceId::Swap(swap_id),
        peer_service: ServiceId::Loopback,
        chain,
        state: init_state,
        funding_outpoint: default!(),
        maker_peer: None,
        started: SystemTime::now(),
        accordant_amount,
        arbitrating_amount,
        cancel_timelock,
        punish_timelock,
        fee_strategy,
        accordant_blockchain,
        arbitrating_blockchain,
        network,
        enquirer: None,
        tx_finality_thr: 1,
        storage: Box::new(storage::DiskDriver::init(
            swap_id,
            Box::new(storage::DiskConfig {
                path: Default::default(),
            }),
        )?),
        confirmation_bound: 10000,
        // TODO: query syncer to set this value (start with None)
        task_lifetime: Some(2066175 + 10000),
        txs_status: none!(),
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
    funding_outpoint: OutPoint,
    maker_peer: Option<NodeAddr>,
    started: SystemTime,
    accordant_amount: monero::Amount,
    arbitrating_amount: bitcoin::Amount,
    cancel_timelock: CSVTimelock,
    punish_timelock: CSVTimelock,
    fee_strategy: FeeStrategy<SatPerVByte>,
    network: blockchain::Network,
    arbitrating_blockchain: Bitcoin<SegwitV0>,
    accordant_blockchain: Monero,
    enquirer: Option<ServiceId>,
    tx_finality_thr: i32,
    confirmation_bound: u16,
    task_lifetime: Option<u64>,
    txs_status: HashMap<i32, (TxLabel, TxStatus)>,
    pending_requests: HashMap<ServiceId, PendingRequest>,
    #[allow(dead_code)]
    storage: Box<dyn storage::Driver>,
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
    RefundProcedureSignatures,
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
    CorearbB(PartiallySignedTransaction), // lock
    #[display("BuyProcSig")]
    BuyProcSigB,
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
                    Msg::MakerCommit(remote_commit) => {
                        // we are taker and the maker committed, now we reveal after checking
                        // whether we're Bob or Alice and that we're on a compatible state
                        trace!("received commitment from counterparty, can now reveal");
                        let (next_state, local_params) = match self.state.clone() {
                            State::Alice(AliceState::CommitA(_, local_params, _, None)) => Ok((
                                State::Alice(AliceState::RevealA(remote_commit.clone())),
                                local_params,
                            )),
                            State::Bob(BobState::CommitB(_, local_params, _, None, addr)) => {
                                let tx_label = TxLabel::Funding;
                                let (watch_addr_req, id) =
                                    watch_addr(addr, self.task_lifetime.expect("task_lifetime"));
                                self.txs_status.insert(id, (tx_label, TxStatus::Notfinal));
                                // deferred to when syncer comes online
                                // FIXME
                                self.send_ctl(senders, ServiceId::Syncer, watch_addr_req)?;
                                Ok((
                                    State::Bob(BobState::RevealB(remote_commit.clone())),
                                    local_params,
                                ))
                            }
                            _ => Err(Error::Farcaster("Must be on Commit state".to_string())),
                        }?;
                        let reveal: Reveal = (msg.swap_id(), local_params.clone()).into();
                        self.send_wallet(msg_bus, senders, request)?;
                        self.send_peer(senders, Msg::Reveal(reveal))?;
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
                                let tx_label = TxLabel::Funding;
                                let (watch_addr_req, id) =
                                    watch_addr(addr, self.task_lifetime.expect("task_lifetime"));
                                self.txs_status.insert(id, (tx_label, TxStatus::Notfinal));

                                // FIXME
                                // deferred to when syncer comes online
                                // self.pending_requests
                                //     .insert(ServiceId::Syncer, vec![watch_addr]);
                                self.send_ctl(senders, ServiceId::Syncer, watch_addr_req)?;
                                Ok((
                                    State::Bob(BobState::RevealB(remote_commit.clone())),
                                    remote_commit,
                                ))
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
                            _ => Err(Error::Farcaster(
                                "Must be on Commit or Reveal state".to_string(),
                            )),
                        }?;

                        // parameter processing irrespective of maker & taker role
                        let core_wallet = CoreWallet::new_keyless();
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
                                if let None = self
                                    .pending_requests
                                    .insert(ServiceId::Wallet, pending_request)
                                {
                                    trace!("Pending request will be called later")
                                } else {
                                    error!("A pending request was removed, FIXME")
                                }
                            }
                        }
                        // up to here for both maker and taker, following only Maker

                        // if did not yet reveal, maker only. on the msg flow as
                        // of 2021-07-13 taker reveals first
                        match &self.state {
                            State::Alice(AliceState::CommitA(
                                TradeRole::Maker,
                                local_params,
                                ..,
                            ))
                            | State::Bob(BobState::CommitB(TradeRole::Maker, local_params, ..)) => {
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
                                let id = task_id(txid);
                                if self
                                    .txs_status
                                    .insert(id, (tx_label.clone(), TxStatus::Notfinal))
                                    .is_none()
                                {
                                    let task = Task::WatchTransaction(WatchTransaction {
                                        id,
                                        lifetime: self
                                            .task_lifetime
                                            .expect("task_lifetime is None"),
                                        hash: txid.to_vec(),
                                        confirmation_bound: self.confirmation_bound,
                                    });
                                    senders.send_to(
                                        ServiceBus::Ctl,
                                        self.identity(),
                                        ServiceId::Syncer,
                                        Request::SyncerTask(task),
                                    )?;
                                }
                            }
                            self.send_wallet(msg_bus, senders, request.clone())?
                        } else {
                            Err(Error::Farcaster(s!(
                                "Wrong state: Only Alice receives CoreArbitratingSetup msg \\
                                 through peer connection at state RevealA"
                            )))?
                        }
                    }
                    // bob receives, alice sends
                    Msg::RefundProcedureSignatures(_) => {
                        if let State::Bob(BobState::CorearbB(lock)) = self.state.clone() {
                            // FIXME subscribe syncer to Accordant + arbitrating locks and buy +
                            // cancel txs
                            let lock_tx = lock.extract_tx();
                            let mut tx = Vec::new();
                            let mut writer = Cursor::new(&mut tx);
                            lock_tx.consensus_encode(&mut writer)?;
                            let id = task_id(lock_tx.txid());
                            let broadcast_arb_lock =
                                Task::BroadcastTransaction(BroadcastTransaction { id, tx });
                            info!("Broadcasting arbitrating lock {}", broadcast_arb_lock);
                            senders.send_to(
                                ServiceBus::Ctl,
                                self.identity(),
                                ServiceId::Syncer,
                                Request::SyncerTask(broadcast_arb_lock),
                            )?;
                            self.send_wallet(msg_bus, senders, request.clone())?
                        } else {
                            Err(Error::Farcaster(
                                "Wrong state: Bob receives RefundProcedureSignatures msg \\
                                 through peer connection in state CorearbB"
                                    .to_string(),
                            ))?
                        }
                    }
                    // alice receives, bob sends
                    Msg::BuyProcedureSignature(BuyProcedureSignature { buy, .. }) => {
                        // Alice verifies that she has sent refund procedure signatures before
                        // processing the buy signatures from Bob
                        if let State::Alice(AliceState::RefundProcedureSignatures) = self.state {
                            let txid = buy.clone().extract_tx().txid();
                            let id = task_id(txid);
                            if self
                                .txs_status
                                .insert(id, (TxLabel::Buy, TxStatus::Notfinal))
                                .is_none()
                            {
                                // notify the syncer to watch for the buy transaction
                                let task = Task::WatchTransaction(WatchTransaction {
                                    id,
                                    lifetime: self.task_lifetime.expect("task_lifetime is None"),
                                    hash: txid.to_vec(),
                                    confirmation_bound: self.confirmation_bound,
                                });
                                senders.send_to(
                                    ServiceBus::Ctl,
                                    self.identity(),
                                    ServiceId::Syncer,
                                    Request::SyncerTask(task),
                                )?;
                                self.send_wallet(msg_bus, senders, request.clone())?
                            } else {
                                error!("Task already registered for this swapd");
                                return Ok(());
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
            // Request::PeerMessage(Messages::FundingCreated(funding_created))
            // => {     let enquirer = self.enquirer.clone();

            //     let funding_signed =
            //         self.funding_created(senders, funding_created)?;

            //     // self.send_peer(
            //     //     senders,
            //     //     Messages::FundingSigned(funding_signed),
            //     // )?;

            //     // Ignoring possible error here: do not want to
            //     // halt the channel just because the client disconnected
            //     let msg = format!(
            //         "{} both signatures present",
            //         "Channel funded:".bright_green_bold()
            //     );
            //     info!("{}", msg);
            //     let _ = self.report_progress_to(senders, &enquirer, msg);
            // }
            Request::PeerMessage(Messages::FundingLocked(_funding_locked)) => {
                let enquirer = self.enquirer.clone();

                // self.state = Lifecycle::Locked;

                // TODO:
                //      1. Change the channel state
                //      2. Do something with per-commitment point

                // self.state = Lifecycle::Active;
                // self.remote_capacity = self.params.funding_satoshis;

                // Ignoring possible error here: do not want to
                // halt the channel just because the client disconnected
                let msg = format!(
                    "{} transaction confirmed",
                    "Channel active:".bright_green_bold()
                );
                info!("{}", msg);
                let _ = self.report_success_to(senders, &enquirer, Some(msg));
            }

            Request::PeerMessage(Messages::CommitmentSigned(_commitment_signed)) => {}

            Request::PeerMessage(Messages::RevokeAndAck(_revoke_ack)) => {}

            Request::PeerMessage(_) => {
                // Ignore the rest of LN peer messages
            }

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
            (Request::Hello, ServiceId::Syncer) => {
                info!("Source: {} is connected", source);
            }

            (Request::Hello, _) => {
                info!("Source: {} is connected", source);
            }
            (_, ServiceId::Farcasterd | ServiceId::Wallet | ServiceId::Syncer) => {}
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
                    if let Request::Protocol(Msg::Reveal(Reveal::Bob(_))) = &request {
                        trace!(
                            "sending request {} to {} on bus {}",
                            &request,
                            &dest,
                            &bus_id
                        );
                        senders.send_to(bus_id, source, dest, request)?
                    } {
                        error!("Not the expected request")
                    }
                } else {
                    error!("pending request not found")
                }
            }
            Request::SyncerEvent(ref event) => match &event {
                Event::HeightChanged(h) => {
                    info!("height changed {}", h)
                }
                Event::AddressTransaction(AddressTransaction {
                    id,
                    hash,
                    amount,
                    block,
                    tx,
                }) => {
                    // FIXME where to wait for a few confs?
                    info!("Received AddressTransaction, processing {:?}: ", tx);
                    if let Some((txlabel, status)) = self.txs_status.get_mut(&id) {
                        match txlabel {
                            TxLabel::Funding => {
                                info!("Funding transaction received, forwarding to wallet");
                                self.send_wallet(ServiceBus::Ctl, senders, request)?
                            }
                            _ => {
                                error!("event not funding tx")
                            }
                        }
                    } else {
                        error!("Transaction about unknow transaction");
                    }
                }
                Event::TransactionConfirmations(TransactionConfirmations {
                    id,
                    block,
                    confirmations,
                }) if confirmations >= &self.tx_finality_thr => {
                    if let Some((txlabel, status)) = self.txs_status.get_mut(&id) {
                        *status = TxStatus::Final;
                        info!(
                            "Transaction {} is now final after {} confirmations",
                            txlabel, confirmations
                        );
                        // FIXME: fill match arms
                        match txlabel {
                            TxLabel::Funding => {}
                            TxLabel::Lock => {}
                            TxLabel::Buy => {}
                            TxLabel::Cancel => {}
                            TxLabel::Refund => {}
                            TxLabel::Punish => {}
                        }
                    }
                    info!(
                        "tx {} is now final after {} confirmations",
                        id, confirmations
                    );
                    // TODO abort task, inject into final_txs state
                }
                Event::TransactionConfirmations(TransactionConfirmations {
                    id,
                    block,
                    confirmations,
                }) => {
                    info!("tx {} now has {} confirmations", id, confirmations);
                }
                Event::TransactionBroadcasted(_) => {}
                Event::TaskAborted(_) => {}
            },
            Request::Protocol(Msg::CoreArbitratingSetup(core_arb_setup)) => {
                let next_state = match self.state {
                    State::Bob(BobState::RevealB(_)) => {
                        Ok(State::Bob(BobState::CorearbB(core_arb_setup.lock.clone())))
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
                    let id = task_id(txid);
                    if self
                        .txs_status
                        .insert(id, (tx_label, TxStatus::Notfinal))
                        .is_none()
                    {
                        let task = Task::WatchTransaction(WatchTransaction {
                            id: task_id(txid),
                            lifetime: self.task_lifetime.expect("task_lifetime is None"),
                            hash: txid.to_vec(),
                            confirmation_bound: self.confirmation_bound,
                        });
                        senders.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            ServiceId::Syncer,
                            Request::SyncerTask(task),
                        )?;
                    }
                }
                trace!("sending peer CoreArbitratingSetup msg: {}", &core_arb_setup);
                self.send_peer(senders, Msg::CoreArbitratingSetup(core_arb_setup))?;
                info!("State transition: {}", next_state.bright_blue_bold());
                self.state = next_state;
            }

            Request::Protocol(Msg::RefundProcedureSignatures(refund_proc_sigs)) => {
                // must have received params before
                let next_state = match self.state {
                    State::Alice(AliceState::RevealA(_)) => {
                        Ok(State::Alice(AliceState::RefundProcedureSignatures))
                    }
                    _ => Err(Error::Farcaster(s!("Wrong state: must be RevealA"))),
                }?;
                trace!("sending peer RefundProcedureSignatures msg");
                self.send_peer(senders, Msg::RefundProcedureSignatures(refund_proc_sigs))?;
                info!("State transition: {}", next_state.bright_blue_bold());
                self.state = next_state;
            }

            Request::Protocol(Msg::BuyProcedureSignature(buy_proc_sig)) => {
                let next_state = match self.state {
                    State::Bob(BobState::CorearbB(..)) => Ok(State::Bob(BobState::BuyProcSigB)),
                    _ => Err(Error::Farcaster(s!("Wrong state: must be CorearbB "))),
                }?;

                let txid = buy_proc_sig.buy.clone().extract_tx().txid();
                let task = Task::WatchTransaction(WatchTransaction {
                    id: task_id(txid),
                    lifetime: self.task_lifetime.expect("task_lifetime is None"),
                    hash: txid.to_vec(),
                    confirmation_bound: self.confirmation_bound,
                });
                senders.send_to(
                    ServiceBus::Ctl,
                    self.identity(),
                    ServiceId::Syncer,
                    Request::SyncerTask(task),
                )?;

                trace!("sending peer BuyProcedureSignature msg");
                self.send_peer(senders, Msg::BuyProcedureSignature(buy_proc_sig))?;
                info!("State transition: {}", next_state.bright_blue_bold());
                self.state = next_state;
            }
            // Request::FundSwap(funding_outpoint) => {
            //     self.enquirer = source.into();

            //     let funding_created =
            //         self.fund_swap(senders, funding_outpoint)?;

            //     // self.state = Lifecycle::Funding;
            //     // self.send_peer(
            //     //     senders,
            //     //     Messages::FundingCreated(funding_created),
            //     // )?;
            // }
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
        let core_wallet = CoreWallet::new_keyless();
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

        // self.params = payment::channel::Params::with(&swap_req)?;
        // self.local_keys = payment::channel::Keyset::from(swap_req);

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

        let core_wallet = CoreWallet::new_keyless();
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

    // pub fn fund_swap(
    //     &mut self,
    //     senders: &mut Senders,
    //     funding_outpoint: OutPoint,
    // ) -> Result<message::FundingCreated, Error> {
    //     let enquirer = self.enquirer.clone();

    //     info!(
    //         "{} {}",
    //         "Funding channel".bright_blue_bold(),
    //         self.swap_id.bright_blue_italic()
    //     );
    //     let _ = self.report_progress_to(
    //         senders,
    //         &enquirer,
    //         format!("Funding channel {:#}", self.swap_id),
    //     );

    //     self.funding_outpoint = funding_outpoint;
    //     // self.funding_update(senders)?;

    //     let signature = self.sign_funding();
    //     let funding_created = message::FundingCreated {
    //         temporary_channel_id: self.swap_id.into(),
    //         funding_txid: self.funding_outpoint.txid,
    //         funding_output_index: self.funding_outpoint.vout as u16,
    //         signature,
    //     };
    //     trace!("Prepared funding_created: {:?}", funding_created);

    //     let msg = format!(
    //         "{} for channel {:#}. Awaiting for remote node signature.",
    //         "Funding created".bright_green_bold(),
    //         self.swap_id.bright_green_italic()
    //     );
    //     info!("{}", msg);
    //     let _ = self.report_progress_to(senders, &enquirer, msg);

    //     Ok(funding_created)
    // }

    // pub fn funding_created(
    //     &mut self,
    //     senders: &mut Senders,
    //     funding_created: message::FundingCreated,
    // ) -> Result<message::FundingSigned, Error> {
    //     let enquirer = self.enquirer.clone();

    //     info!(
    //         "{} {}",
    //         "Accepting channel funding".bright_blue_bold(),
    //         self.swap_id.bright_blue_italic()
    //     );
    //     let _ = self.report_progress_to(
    //         senders,
    //         &enquirer,
    //         format!(
    //             "Accepting channel funding {:#}",
    //             self.swap_id
    //         ),
    //     );

    //     self.funding_outpoint = OutPoint {
    //         txid: funding_created.funding_txid,
    //         vout: funding_created.funding_output_index as u32,
    //     };
    //     // TODO: Save signature!
    //     self.funding_update(senders)?;

    //     let signature = self.sign_funding();
    //     let funding_signed = message::FundingSigned {
    //         channel_id: self.swap_id,
    //         signature,
    //     };
    //     trace!("Prepared funding_signed: {:?}", funding_signed);

    //     let msg = format!(
    //         "{} for channel {:#}. Awaiting for funding tx mining.",
    //         "Funding signed".bright_green_bold(),
    //         self.swap_id.bright_green_italic()
    //     );
    //     info!("{}", msg);
    //     let _ = self.report_progress_to(senders, &enquirer, msg);

    //     Ok(funding_signed)
    // }

    // pub fn sign_funding(&mut self) -> secp256k1::Signature {
    //     // We are doing counterparty's transaction!
    //     let mut cmt_tx = Transaction::ln_cmt_base(
    //         100,
    //         100,
    //         100,
    //         self.obscuring_factor,
    //         self.funding_outpoint,
    //         self.local_keys.payment_basepoint,
    //         self.local_keys.revocation_basepoint,
    //         self.remote_keys.delayed_payment_basepoint,
    //         100,
    //     );
    //     trace!("Counterparty's commitment tx: {:?}", cmt_tx);

    //     let mut sig_hasher = SigHashCache::new(&mut cmt_tx);
    //     let sighash = sig_hasher.signature_hash(
    //         0,
    //         &PubkeyScript::ln_funding(
    //             self.channel_capacity(),
    //             self.local_keys.funding_pubkey,
    //             self.remote_keys.funding_pubkey,
    //         )
    //         .into(),
    //         self.channel_capacity(),
    //         SigHashType::All,
    //     );
    //     let sign_msg = secp256k1::Message::from_slice(&sighash[..])
    //         .expect("Sighash size always match requirements");
    //     let signature = self.local_node.sign(&sign_msg);
    //     trace!("Commitment transaction signature created");
    //     // .serialize_der();
    //     // let mut with_hashtype = signature.to_vec();
    //     // with_hashtype.push(SigHashType::All.as_u32() as u8);

    //     signature
    // }
}

pub fn swap_id(source: ServiceId) -> Result<SwapId, Error> {
    if let ServiceId::Swap(swap_id) = source {
        Ok(swap_id)
    } else {
        Err(Error::Farcaster("Not swapd".to_string()))
    }
}

pub fn task_id(txid: Txid) -> i32 {
    let buf: [u8; 4] = txid.to_vec()[0..4].try_into().expect("task_id");
    i32::from_le_bytes(buf)
}

enum TxStatus {
    Final,
    Notfinal,
}

fn watch_addr(addr: bitcoin::Address, lifetime: u64) -> (Request, i32) {
    let addendum = BtcAddressAddendum {
        address: addr.to_string(),
        from_height: 0,
        script_pubkey: addr.script_pubkey().to_bytes(),
    };
    let addendum = consensus::serialize(&addendum);
    let buf: Vec<u8> = addendum.clone().try_into().expect("watch_addr");
    let id_buf: [u8; 4] = keccak_256(&buf)[0..4].try_into().expect("watch_addr");
    let id = i32::from_le_bytes(id_buf);

    (
        Request::SyncerTask(Task::WatchAddress(WatchAddress {
            id,
            lifetime,
            addendum,
            include_tx: Boolean::True,
        })),
        id,
    )
}

struct PendingRequest {
    request: Request,
    dest: ServiceId,
    bus_id: ServiceBus,
}
