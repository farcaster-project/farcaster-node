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

use std::convert::TryFrom;
use std::time::{Duration, SystemTime};
use std::{collections::BTreeMap, convert::TryInto};

use super::storage::{self, Driver};
use crate::rpc::{
    request::{self, Msg},
    Request, ServiceBus,
};
use crate::{Config, CtlServer, Error, LogStyle, Senders, Service, ServiceId};
use bitcoin::hashes::{sha256, Hash, HashEngine};
use bitcoin::secp256k1;
use bitcoin::util::bip143::SigHashCache;
use bitcoin::{OutPoint, SigHashType, Transaction};

use farcaster_core::{
    chain::bitcoin::{fee::SatPerVByte, Bitcoin, timelock::CSVTimelock},
    chain::monero::Monero,
    chain::pairs::btcxmr::{Wallet as CoreWallet, BtcXmr},
    blockchain::{self, FeeStrategy},
    negotiation::{Offer, PublicOffer},
    protocol_message::{CoreArbitratingSetup, CommitAliceParameters, CommitBobParameters},
    role::{Arbitrating, TradeRole, SwapRole},
};
use internet2::zmqsocket::{self, ZmqSocketAddr, ZmqType};
use internet2::{
    session, CreateUnmarshaller, NodeAddr, Session, TypedEnum, Unmarshall, Unmarshaller,
};
use lnp::payment::bolt3::{ScriptGenerators, TxGenerators};
use lnp::payment::htlc::{HtlcKnown, HtlcSecret};
use lnp::payment::{self, AssetsBalance, Lifecycle};
use lnp::{message, ChannelId as SwapId, Messages, TempChannelId as TempSwapId};
use lnpbp::{chain::AssetId, Chain};
use microservices::esb::{self, Handler};
use request::{Commit, InitSwap, Params, Reveal, TakeCommit};

pub fn run(
    config: Config,
    swap_id: SwapId,
    chain: Chain,
    public_offer: PublicOffer<BtcXmr>,
    negotiation_role: TradeRole,
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
    let local_role = match negotiation_role {
        TradeRole::Maker => maker_role,
        TradeRole::Taker => maker_role.other(),
    };

    let init_state = |&role| match role {
        SwapRole::Alice => State::Alice(AliceState::StartA),
        SwapRole::Bob => State::Bob(BobState::StartB),
    };

    let runtime = Runtime {
        identity: ServiceId::Swap(swap_id),
        peer_service: ServiceId::Loopback,
        chain,
        state: init_state(&local_role),
        // remote_state: init_state(local_role.other()),
        funding_outpoint: default!(),
        maker_peer: None,
        commit_remote: None,
        commit_local: None,
        local_role,
        started: SystemTime::now(),
        local_params: default!(),
        remote_params: default!(),
        is_maker: false,
        obscuring_factor: 0,
        accordant_amount,
        arbitrating_amount,
        cancel_timelock,
        punish_timelock,
        fee_strategy,
        accordant_blockchain,
        arbitrating_blockchain,
        network,
        public_offer,
        enquirer: None,
        storage: Box::new(storage::DiskDriver::init(
            swap_id,
            Box::new(storage::DiskConfig {
                path: Default::default(),
            }),
        )?),
    };
    let broker = false;
    Service::run(config, runtime, broker)
}
pub struct Runtime {
    identity: ServiceId,
    peer_service: ServiceId,
    chain: Chain,
    state: State,
    // remote_state: State,
    funding_outpoint: OutPoint,
    maker_peer: Option<NodeAddr>,
    started: SystemTime,
    local_params: Option<Params>,
    remote_params: Option<Params>,
    is_maker: bool,
    obscuring_factor: u64,
    commit_remote: Option<Commit>,
    commit_local: Option<Commit>,
    local_role: SwapRole,
    accordant_amount: monero::Amount,
    arbitrating_amount: bitcoin::Amount,
    cancel_timelock: CSVTimelock,
    punish_timelock: CSVTimelock,
    fee_strategy: FeeStrategy<SatPerVByte>,
    network: blockchain::Network,
    arbitrating_blockchain: Bitcoin,
    accordant_blockchain: Monero,
    public_offer: PublicOffer<BtcXmr>, // TODO: replace by pub offer id
    enquirer: Option<ServiceId>,
    #[allow(dead_code)]
    storage: Box<dyn storage::Driver>,
}

#[derive(Eq, PartialEq)]
pub enum AliceState {
    StartA,
    CommitA,
    RevealA,
    RefundProcedureSignatures,
    FinishA,
}

#[derive(Eq, PartialEq)]
pub enum BobState {
    StartB,
    CommitB,
    RevealB,
    CorearbB,
    BuyProcSigB,
    FinishB,
}

#[derive(Eq, PartialEq)]
pub enum State {
    Alice(AliceState),
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
        trace!("sending peer message {}", msg.amount());
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
                match &msg {
                    Msg::MakerCommit(commit) => {
                        // bob and alice, maker commited, now taker reveals
                        trace!("received commitment from counterparty, can now reveal");
                        self.commit_remote = Some(commit.clone());
                        if let Some(local_params) = &self.local_params {
                            let reveal: Reveal = self
                                .local_params
                                .clone()
                                .map(Into::into)
                                .ok_or_else(|| {
                                    Error::Farcaster("Failed to construct Reveal".to_string())
                                })?;
                            self.send_peer(senders, Msg::Reveal(reveal))?
                        } else {
                            Err(Error::Farcaster(s!("local_params is None, did not reveal")))?
                        };
                    }
                    Msg::TakerCommit(TakeCommit { commit, .. }) => {
                        unreachable!(
                            "msg handled by farcasterd/walletd, and indirectly here by Request::MakeSwap"
                        )
                    }
                    // bob and alice
                    Msg::Reveal(role) => {
                        if self.remote_params.is_some() {
                            Err(Error::Farcaster("remote_params already set".to_string()))?
                        }

                        let next_state = match self.state {
                            State::Alice(AliceState::CommitA) => {
                                Ok(State::Alice(AliceState::RevealA))
                            }
                            State::Bob(BobState::CommitB) => Ok(State::Bob(BobState::RevealB)),
                            State::Alice(AliceState::RevealA) => {
                                Ok(State::Alice(AliceState::RevealA))
                            }
                            State::Bob(BobState::RevealB) => Ok(State::Bob(BobState::RevealB)),
                            _ => Err(Error::Farcaster(
                                "Must be on Commit state (maker) or reveal state (taker)"
                                    .to_string(),
                            )),
                        }?;

                        let core_wallet = CoreWallet::new_keyless();
                        let remote_params = match role {
                            Reveal::Alice(reveal) => match &self.commit_remote {
                                Some(Commit::Alice(commit)) => {
                                    commit.verify_with_reveal(&core_wallet, reveal.clone())?;
                                    Params::Alice(reveal.clone().into())
                                }
                                _ => {
                                    let err_msg = "expected Some(Commit::Alice(commit))";
                                    error!("{}", err_msg);
                                    Err(Error::Farcaster(err_msg.to_string()))?
                                }
                            },
                            Reveal::Bob(reveal) => match &self.commit_remote {
                                Some(Commit::Bob(commit)) => {
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
                        self.remote_params = Some(remote_params.clone());
                        // self.send_peer(senders, msg)?;
                        self.send_wallet(msg_bus, senders, Request::Params(remote_params))?;

                        // if did not yet reveal, maker only. on the msg flow as
                        // of 2021-07-13 taker reveals first
                        if self.state == State::Alice(AliceState::CommitA)
                            || self.state == State::Bob(BobState::CommitB)
                        {
                            if let Some(local_params) = &self.local_params {
                                let reveal: Reveal = local_params.clone().into();
                                self.send_peer(senders, Msg::Reveal(reveal))?
                            } else {
                                // maybe should not fail here because of the
                                Err(Error::Farcaster(s!("local_params is None, did not reveal")))?
                            };
                        }
                        self.state = next_state;
                    }
                    // alice receives, bob sends
                    Msg::CoreArbitratingSetup(_) => {
                        if self.state != State::Alice(AliceState::RevealA) {
                            Err(Error::Farcaster(s!(
                                "Wrong state: Only Alice receives CoreArbitratingSetup msg \\
                                 through peer connection at state RevealA"
                            )))?
                        }
                        self.send_wallet(msg_bus, senders, request.clone())?
                    }
                    // bob receives, alice sends
                    Msg::RefundProcedureSignatures(_) => {
                        if self.state != State::Bob(BobState::CorearbB) {
                            Err(Error::Farcaster(
                                "Wrong state: Bob receives RefundProcedureSignatures msg \\
                                 through peer connection in state CorearbB"
                                    .to_string(),
                            ))?
                        }
                        self.send_wallet(msg_bus, senders, request.clone())?
                    }
                    // alice receives, bob sends
                    // ProtocolMessages::BuyProcedureSignature(_) => {}
                    Msg::BuyProcedureSignature(_) => {
                        if self.state == State::Alice(AliceState::RefundProcedureSignatures) {
                            Err(Error::Farcaster(s!(
                                "Wrong state: must be RefundProcedureSignatures"
                            )))?
                        }
                        self.send_wallet(msg_bus, senders, request.clone())?
                    }

                    // bob and alice
                    Msg::Abort(_) => Err(Error::Farcaster("Abort not yet supported".to_string()))?,
                    Msg::Ping => { unreachable!("ping must remain in peerd, not arrive in swapd") },
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
            //         "Channel funded:".ended()
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
                let msg = format!("{} transaction confirmed", "Channel active:".ended());
                info!("{}", msg);
                let _ = self.report_success_to(senders, &enquirer, Some(msg));
            }

            Request::PeerMessage(Messages::CommitmentSigned(_commitment_signed)) => {}

            Request::PeerMessage(Messages::RevokeAndAck(_revoke_ack)) => {}

            Request::PeerMessage(_) => {
                // Ignore the rest of LN peer messages
            }

            _ => {
                error!("MSG RPC can be only used for forwarding LNPWP messages");
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
        if source != ServiceId::Farcasterd {
            Err(Error::Farcaster(
                "Permission Error: only Farcasterd can can control swapd".to_string(),
            ))?
        }
        let ctl_bus = ServiceBus::Ctl;
        match request {
            Request::TakeSwap(InitSwap {
                peerd,
                report_to,
                params,
                swap_id,
                commit: None,
            }) => {
                if &ServiceId::Swap(swap_id) != &self.identity {
                    error!(
                        "{}: {}",
                        "This swapd instance is not reponsible for swap_id", swap_id
                    )
                };
                let next_state = match self.state {
                    State::Bob(BobState::StartB) => Ok(State::Bob(BobState::CommitB)),
                    State::Alice(AliceState::StartA) => Ok(State::Alice(AliceState::CommitA)),
                    _ => Err(Error::Farcaster(s!("Wrong state: Expects Start state"))),
                }?;
                self.peer_service = peerd.clone();
                self.enquirer = report_to.clone();
                self.local_params = Some(params.clone());

                if let ServiceId::Peer(ref addr) = peerd {
                    self.maker_peer = Some(addr.clone());
                }
                let commit = self.taker_commit(senders, params).map_err(|err| {
                    self.report_failure_to(
                        senders,
                        &report_to,
                        microservices::rpc::Failure {
                            code: 0, // TODO: Create error type system
                            info: err.to_string(),
                        },
                    )
                })?;
                let public_offer_hex = self.public_offer.to_string();
                let take_swap = TakeCommit {
                    commit,
                    public_offer_hex,
                    swap_id,
                };
                self.send_peer(senders, Msg::TakerCommit(take_swap))?;
                self.state = next_state;
            }

            Request::MakeSwap(InitSwap {
                peerd,
                report_to,
                params,
                swap_id,
                commit: Some(remote_commit),
            }) => {
                if self.commit_remote.is_some() {
                    Err(Error::Farcaster("remote commit already set".to_string()))?
                }
                let next_state = match self.state {
                    State::Bob(BobState::StartB) => Ok(State::Bob(BobState::CommitB)),
                    State::Alice(AliceState::StartA) => Ok(State::Alice(AliceState::CommitA)),
                    _ => Err(Error::Farcaster(s!("Wrong state: Expects Start"))),
                }?;
                self.peer_service = peerd.clone();

                if let ServiceId::Peer(ref addr) = peerd {
                    self.maker_peer = Some(addr.clone());
                }

                let local_commit = self
                    .maker_commit(senders, &peerd, swap_id, params)
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

                trace!("setting commit_remote and commit_local msg");
                self.commit_remote = Some(remote_commit);
                self.commit_local = Some(local_commit.clone());
                trace!("sending peer MakerCommit msg");
                self.send_peer(senders, Msg::MakerCommit(local_commit))?;
                self.state = next_state;
            }

            Request::Protocol(Msg::CoreArbitratingSetup(core_arb_setup)) => {
                let next_state = match self.state {
                    State::Bob(BobState::RevealB) => Ok(State::Bob(BobState::CorearbB)),
                    _ => Err(Error::Farcaster(s!("Wrong state: must be RevealBB"))),
                }?;
                trace!("sending peer CoreArbitratingSetup msg");
                self.send_peer(senders, Msg::CoreArbitratingSetup(core_arb_setup))?;
                self.state = next_state;
            }

            Request::Protocol(Msg::RefundProcedureSignatures(refund_proc_sigs)) => {
                // must have received params before
                let next_state = match self.state {
                    State::Alice(AliceState::RevealA) => {
                        Ok(State::Alice(AliceState::RefundProcedureSignatures))
                    }
                    _ => Err(Error::Farcaster(s!("Wrong state: must be RevealA"))),
                }?;
                if self.remote_params.is_none() {
                    Err(Error::Farcaster(s!("remote_params is none")))?
                }
                trace!("sending peer RefundProcedureSignatures msg");
                self.send_peer(senders, Msg::RefundProcedureSignatures(refund_proc_sigs))?;
                self.state = next_state;
            }

            Request::Protocol(Msg::BuyProcedureSignature(buy_proc_sig)) => {
                let next_state = match self.state {
                    State::Bob(BobState::CorearbB) => Ok(State::Bob(BobState::BuyProcSigB)),
                    _ => Err(Error::Farcaster(s!("Wrong state: must be CorearbB "))),
                }?;

                trace!("sending peer BuyProcedureSignature msg");
                self.send_peer(senders, Msg::BuyProcedureSignature(buy_proc_sig))?;
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
                    assets: none!(),
                    remote_peers: self.maker_peer.clone().map(|p| vec![p]).unwrap_or_default(),
                    uptime: SystemTime::now()
                        .duration_since(self.started)
                        .unwrap_or(Duration::from_secs(0)),
                    since: self
                        .started
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap_or(Duration::from_secs(0))
                        .as_secs(),
                    is_maker: self.is_maker,
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
            "Proposing to the Maker".promo(),
            "that I take the swap offer".promo(),
            self.swap_id().promoter()
        );
        info!("{}", &msg);
        let core_wallet = CoreWallet::new_keyless();
        let commitment = match params.clone() {
            Params::Bob(params) => request::Commit::Bob(
                CommitBobParameters::commit_to_bundle(&core_wallet, params)
                ),
            Params::Alice(params) => request::Commit::Alice(
                CommitAliceParameters::commit_to_bundle(&core_wallet, params)
                ),
        };
        // Ignoring possible reporting errors here and after: do not want to
        // halt the swap just because the client disconnected
        let enquirer = self.enquirer.clone();
        let _ = self.report_progress_to(senders, &enquirer, msg)?;

        self.is_maker = false;
        self.local_params = Some(params);
        // self.params = payment::channel::Params::with(&swap_req)?;
        // self.local_keys = payment::channel::Keyset::from(swap_req);

        Ok(commitment)
    }

    pub fn maker_commit(
        &mut self,
        senders: &mut Senders,
        peerd: &ServiceId,
        swap_id: SwapId,
        params: Params,
    ) -> Result<request::Commit, Error> {
        let msg = format!(
            "{} with swap id {:#} from remote peer {}",
            "Accepting channel".promo(),
            swap_id.promoter(),
            peerd.promoter()
        );
        info!("{}", msg);

        // Ignoring possible reporting errors here and after: do not want to
        // halt the channel just because the client disconnected
        let enquirer = self.enquirer.clone();
        let _ = self.report_progress_to(senders, &enquirer, msg);

        self.is_maker = true;
        // self.params = payment::channel::Params::with(channel_req)?;
        // self.remote_keys = payment::channel::Keyset::from(channel_req);

        let core_wallet = CoreWallet::new_keyless();
        let commitment = match params.clone() {
            Params::Bob(params) => request::Commit::Bob(
                CommitBobParameters::commit_to_bundle(&core_wallet, params)
                ),
            Params::Alice(params) => request::Commit::Alice(
                CommitAliceParameters::commit_to_bundle(&core_wallet, params)
                ),
        };
        // let accept_channel = message::AcceptChannel {
        //     temporary_channel_id: channel_req.temporary_channel_id,
        //     dust_limit_satoshis: channel_req.dust_limit_satoshis,
        //     max_htlc_value_in_flight_msat: channel_req
        //         .max_htlc_value_in_flight_msat,
        //     channel_reserve_satoshis: channel_req.channel_reserve_satoshis,
        //     htlc_minimum_msat: channel_req.htlc_minimum_msat,
        //     minimum_depth: 3, // TODO: take from config options
        //     to_self_delay: channel_req.to_self_delay,
        //     max_accepted_htlcs: channel_req.max_accepted_htlcs,
        //     funding_pubkey: dumb_key,
        //     revocation_basepoint: dumb_key,
        //     payment_point: dumb_key,
        //     delayed_payment_basepoint: dumb_key,
        //     htlc_basepoint: dumb_key,
        //     first_per_commitment_point: dumb_key,
        //     /* shutdown_scriptpubkey: None,
        //      * unknown_tlvs: none!(), */
        // };

        // self.params.updated(&accept_channel, None)?;
        // self.local_keys = payment::channel::Keyset::from(&accept_channel);

        let msg = format!(
            "{} swap {:#} from remote peer Taker {}",
            "Making".ended(),
            swap_id.ender(),
            peerd.ender()
        );
        info!("{}", msg);
        let _ = self.report_success_to(senders, &enquirer, Some(msg));
        // self.send_peer(senders, ProtocolMessages::Commit(swap_req.clone()))?;
        Ok(commitment.clone())
    }

    pub fn channel_accepted(
        &mut self,
        senders: &mut Senders,
        accept_channel: &message::AcceptChannel,
        peerd: &ServiceId,
    ) -> Result<(), payment::channel::NegotiationError> {
        info!(
            "Channel {:#} {} by the remote peer {}",
            accept_channel.temporary_channel_id.ender(),
            "was accepted".ended(),
            peerd.ender()
        );
        // Ignoring possible reporting errors here and after: do not want to
        // halt the channel just because the client disconnected
        let enquirer = self.enquirer.clone();
        let _ = self.report_progress_to(
            senders,
            &enquirer,
            "Channel was accepted by the remote peer",
        );

        let msg = format!(
            "{} returned parameters for the channel {:#}",
            "Verifying".promo(),
            accept_channel.temporary_channel_id.promoter()
        );
        info!("{}", msg);

        // TODO: Add a reasonable min depth bound
        // self.params.updated(accept_channel, None)?;
        // self.remote_keys = payment::channel::Keyset::from(accept_channel);

        let msg = format!(
            "Channel {:#} is {}",
            accept_channel.temporary_channel_id.ender(),
            "ready for funding".ended()
        );
        info!("{}", msg);
        let _ = self.report_success_to(senders, &enquirer, Some(msg));

        Ok(())
    }

    // pub fn fund_swap(
    //     &mut self,
    //     senders: &mut Senders,
    //     funding_outpoint: OutPoint,
    // ) -> Result<message::FundingCreated, Error> {
    //     let enquirer = self.enquirer.clone();

    //     info!(
    //         "{} {}",
    //         "Funding channel".promo(),
    //         self.swap_id.promoter()
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
    //         "Funding created".ended(),
    //         self.swap_id.ender()
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
    //         "Accepting channel funding".promo(),
    //         self.swap_id.promoter()
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
    //         "Funding signed".ended(),
    //         self.swap_id.ender()
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
