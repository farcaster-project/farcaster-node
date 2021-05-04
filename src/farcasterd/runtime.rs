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

use amplify::Wrapper;
use request::Params;
use std::convert::TryFrom;
use std::ffi::OsStr;
use std::hash::Hash;
use std::io;
use std::net::SocketAddr;
use std::process;
use std::time::{Duration, SystemTime};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
};

use bitcoin::hashes::hex::ToHex;
use bitcoin::secp256k1;
use internet2::{NodeAddr, RemoteSocketAddr, ToNodeAddr, TypedEnum};
use lnp::{
    message, ChannelId as SwapId, Messages, TempChannelId as TempSwapId, LIGHTNING_P2P_DEFAULT_PORT,
};
use lnpbp::Chain;
use microservices::esb::{self, Handler};
use microservices::rpc::Failure;

use crate::rpc::request::{IntoProgressOrFalure, Msg, NodeInfo, OptionDetails};
use crate::rpc::{request, Request, ServiceBus};
use crate::{Config, Error, LogStyle, Service, ServiceId};

use farcaster_chains::{bitcoin::Bitcoin, monero::Monero, pairs::btcxmr::BtcXmr};
use farcaster_core::{
    blockchain::FeePolitic,
    bundle::{
        AliceParameters, BobParameters, CoreArbitratingTransactions, FundingTransaction,
        SignedArbitratingLock,
    },
    crypto::FromSeed,
    datum::Key,
    negotiation::PublicOffer,
    protocol_message::{
        BuyProcedureSignature, CommitAliceParameters, CommitBobParameters, CoreArbitratingSetup,
        RefundProcedureSignatures,
    },
    role::{Alice, Bob, NegotiationRole, SwapRole},
};

use std::str::FromStr;

pub fn run(config: Config, node_id: secp256k1::PublicKey, seed: [u8; 32]) -> Result<(), Error> {
    let runtime = Runtime {
        identity: ServiceId::Farcasterd,
        node_id,
        chain: config.chain.clone(),
        listens: none!(),
        started: SystemTime::now(),
        connections: none!(),
        running_swaps: none!(),
        spawning_services: none!(),
        making_swaps: none!(),
        taking_swaps: none!(),
        making_offers: none!(),
        wallets: none!(),
        seed,
    };

    Service::run(config, runtime, true)
}

pub enum Wallet {
    Alice(
        Alice<BtcXmr>,
        AliceParameters<BtcXmr>,
        PublicOffer<BtcXmr>,
        Option<BobParameters<BtcXmr>>,
    ),
    Bob(
        Bob<BtcXmr>,
        BobParameters<BtcXmr>,
        PublicOffer<BtcXmr>,
        Option<FundingTransaction<Bitcoin>>,
        Option<AliceParameters<BtcXmr>>,
        Option<CoreArbitratingTransactions<Bitcoin>>,
    ),
}

pub struct Runtime {
    identity: ServiceId,
    node_id: secp256k1::PublicKey,
    chain: Chain,
    listens: HashSet<RemoteSocketAddr>,
    started: SystemTime,
    connections: HashSet<NodeAddr>,
    running_swaps: HashSet<SwapId>,
    spawning_services: HashMap<ServiceId, ServiceId>,
    making_swaps: HashMap<ServiceId, request::InitSwap>,
    taking_swaps: HashMap<ServiceId, request::InitSwap>,
    making_offers: HashSet<PublicOffer<BtcXmr>>,
    wallets: HashMap<SwapId, Wallet>,
    seed: [u8; 32],
}

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
    fn handle_rpc_msg(
        &mut self,
        senders: &mut esb::SenderList<ServiceBus, ServiceId>,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        match request {
            Request::Hello => {
                // Ignoring; this is used to set remote identity at ZMQ level
            }
            // Request::PeerMessage(Messages::OpenChannel(open_swap)) => {
            //     info!("Creating swap by peer request from {}", source);
            //     self.create_swap(source, None, open_swap, true)?;
            // }

            // 1st protocol message received through peer connection, and last
            // handled by farcasterd
            Request::Protocol(Msg::TakerCommit(request::TakeCommit {
                commitment,
                public_offer_hex,
                swap_id,
            })) => {
                let public_offer: PublicOffer<BtcXmr> = FromStr::from_str(&public_offer_hex)
                    .map_err(|_| {
                        Error::Other(
                            "The offer received on peer conection is not parsable".to_string(),
                        )
                    })?;
                if !self.making_offers.contains(&public_offer) {
                    error!(
                        "Unknow offer {}, you are not the maker of that offer, ignoring it",
                        &public_offer
                    );
                } else {
                    trace!(
                        "Offer {} is known, you created it previously, initiating swap with taker",
                        &public_offer
                    );
                    let PublicOffer {
                        version,
                        offer,
                        daemon_service,
                    } = public_offer.clone();
                    let peer = daemon_service
                        .to_node_addr(LIGHTNING_P2P_DEFAULT_PORT)
                        .ok_or_else(|| internet2::presentation::Error::InvalidEndpoint)?;
                    // we are maker
                    let maker = NegotiationRole::Maker;
                    match offer.maker_role {
                        SwapRole::Bob => {
                            let address = bitcoin::Address::from_str(
                                "bc1qesgvtyx9y6lax0x34napc2m7t5zdq6s7xxwpvk",
                            )
                            .expect("Parsable address");
                            let bob = Bob::<BtcXmr>::new(address.into(), FeePolitic::Aggressive);
                            let params =
                                bob.generate_parameters(&self.seed, &self.seed, &public_offer)?;
                            if self.wallets.get(&swap_id).is_none() {
                                self.wallets.insert(
                                    swap_id,
                                    Wallet::Bob(
                                        bob,
                                        params.clone(),
                                        public_offer.clone(),
                                        None,
                                        None,
                                        None,
                                    ),
                                );
                                launch_swapd(
                                    self,
                                    peer.into(),
                                    Some(source),
                                    maker,
                                    public_offer,
                                    Params::Bob(params),
                                    swap_id,
                                )?;
                            } else {
                                Err(Error::Farcaster("Wallet already existed".to_string()))?
                            }
                        }
                        SwapRole::Alice => {
                            let address = bitcoin::Address::from_str(
                                "bc1qesgvtyx9y6lax0x34napc2m7t5zdq6s7xxwpvk",
                            )
                            .expect("Parsable address");
                            let alice: Alice<BtcXmr> =
                                Alice::new(address.into(), FeePolitic::Aggressive);
                            let params =
                                alice.generate_parameters(&self.seed, &self.seed, &public_offer)?;
                            if self.wallets.get(&swap_id).is_none() {
                                self.wallets.insert(
                                    swap_id,
                                    Wallet::Alice(
                                        alice,
                                        params.clone(),
                                        public_offer.clone(),
                                        None,
                                    ),
                                );
                                launch_swapd(
                                    self,
                                    peer.into(),
                                    Some(source),
                                    maker,
                                    public_offer,
                                    Params::Alice(params),
                                    swap_id,
                                )?;
                            } else {
                                error!("Wallet already existed");
                                Err(Error::Farcaster("Wallet already existed".to_string()))?
                            }
                        }
                    };
                }
            }
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
        senders: &mut esb::SenderList<ServiceBus, ServiceId>,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        let mut notify_cli: Vec<(Option<ServiceId>, Request)> = none!();
        match request {
            Request::Hello => {
                // Ignoring; this is used to set remote identity at ZMQ level
                info!("{} daemon is {}", source.ended(), "connected".ended());

                match &source {
                    ServiceId::Farcasterd => {
                        error!(
                            "{}",
                            "Unexpected another farcasterd instance connection".err()
                        );
                    }
                    ServiceId::Peer(connection_id) => {
                        if self.connections.insert(connection_id.clone()) {
                            info!(
                                "Connection {} is registered; total {} \
                                 connections are known",
                                connection_id,
                                self.connections.len()
                            );
                        } else {
                            warn!(
                                "Connection {} was already registered; the \
                                 service probably was relaunched",
                                connection_id
                            );
                        }
                    }
                    ServiceId::Swap(swap_id) => {
                        if self.running_swaps.insert(swap_id.clone()) {
                            info!(
                                "Swap {} is registered; total {} \
                                 swaps are known",
                                swap_id,
                                self.running_swaps.len()
                            );
                        } else {
                            warn!(
                                "Swap {} was already registered; the \
                                 service probably was relaunched",
                                swap_id
                            );
                        }
                    }
                    _ => {
                        // Ignoring the rest of daemon/client types
                    }
                }

                if let Some(swap_params) = self.making_swaps.get(&source) {
                    // Tell swapd swap options and link it with the
                    // connection daemon
                    debug!(
                        "Swapd {} is known: we spawned it to create a swap. \
                         Ordering swap opening",
                        source
                    );
                    notify_cli.push((
                        swap_params.report_to.clone(),
                        Request::Progress(format!("Swap daemon {} operational", source)),
                    ));
                    senders.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        source.clone(),
                        Request::MakeSwap(swap_params.clone()),
                    )?;
                    self.running_swaps.insert(swap_params.swap_id);
                    self.making_swaps.remove(&source);
                } else if let Some(swap_params) = self.taking_swaps.get(&source) {
                    // Tell swapd swap options and link it with the
                    // connection daemon
                    debug!(
                        "Daemon {} is known: we spawned it to create a swap. \
                         Ordering swap acceptance",
                        source
                    );
                    senders.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        source.clone(),
                        Request::TakeSwap(swap_params.clone()),
                    )?;
                    self.running_swaps.insert(swap_params.swap_id);
                    self.taking_swaps.remove(&source);
                } else if let Some(enquirer) = self.spawning_services.get(&source) {
                    debug!(
                        "Daemon {} is known: we spawned it to create a new peer \
                         connection by a request from {}",
                        source, enquirer
                    );
                    notify_cli.push((
                        Some(enquirer.clone()),
                        Request::Success(OptionDetails::with(format!(
                            "Peer connected to {}",
                            source
                        ))),
                    ));
                    self.spawning_services.remove(&source);
                }
            }

            Request::Params(params) => {
                let swap_id = if let ServiceId::Swap(swap_id) = source {
                    Ok(swap_id)
                } else {
                    Err(Error::Farcaster("Unknown swap_id".to_string()))
                }?;
                match params {
                    // getting paramaters from counterparty alice routed through
                    // swapd, thus im bob on this swap
                    Params::Alice(params) => {
                        match self.wallets.get_mut(&swap_id) {
                            Some(Wallet::Bob(
                                bob,
                                bob_params,
                                public_offer,
                                // TODO: set this somewhere, its now actually None, so will never
                                // hit this.
                                Some(funding_bundle),
                                alice_params, // None
                                core_arb_txs, // None
                            )) => {
                                if alice_params.is_some() {
                                    Err(Error::Farcaster("Alice params already set".to_string()))?
                                }
                                if core_arb_txs.is_some() {
                                    Err(Error::Farcaster("Core Arb Txs already set".to_string()))?
                                }
                                // set wallet params
                                *alice_params = Some(params.clone());
                                // TODO figure out how to create and get funding tx here
                                // let funding_bundle: FundingTransaction<Bitcoin> = todo!();
                                let core_arbitrating_txs = bob.core_arbitrating_transactions(
                                    &params,
                                    bob_params,
                                    &funding_bundle,
                                    public_offer,
                                )?;
                                // set wallet core_arb_txs
                                *core_arb_txs = Some(core_arbitrating_txs.clone());
                                let cosign_arbitrating_cancel = bob
                                    .cosign_arbitrating_cancel(&self.seed, &core_arbitrating_txs)?;
                                let core_arb_setup = CoreArbitratingSetup::<BtcXmr>::from_bundles(
                                    &core_arbitrating_txs,
                                    &cosign_arbitrating_cancel,
                                )?;
                                let core_arb_setup = Msg::CoreArbitratingSetup(core_arb_setup);
                                senders.send_to(
                                    ServiceBus::Ctl,
                                    ServiceId::Farcasterd, // source
                                    source,                // destination swapd
                                    Request::Protocol(core_arb_setup),
                                )?
                            }
                            _ => Err(Error::Farcaster("only Some(Wallet::Bob)".to_string()))?,
                        }
                    }

                    // getting paramaters from counterparty bob, thus im alice
                    // on this swap
                    Params::Bob(params) => match self.wallets.get_mut(&swap_id) {
                        Some(Wallet::Alice(_alice, _alice_params, _public_offer, bob_params)) => {
                            *bob_params = Some(params);
                        }
                        _ => Err(Error::Farcaster("only Some(Wallet::Alice)".to_string()))?,
                    },
                }
            }

            Request::Protocol(Msg::CoreArbitratingSetup(core_arb_setup)) => {
                let swap_id = if let ServiceId::Swap(swap_id) = source {
                    Ok(swap_id)
                } else {
                    Err(Error::Farcaster("Not swapd".to_string()))
                }?;
                let core_arb_txs = core_arb_setup.into_core_transactions();
                match self.wallets.get(&swap_id) {
                    Some(Wallet::Alice(
                        alice,
                        alice_params,
                        public_offer,
                        Some(bob_parameters),
                    )) => {
                        let signed_adaptor_refund = alice.sign_adaptor_refund(
                            &self.seed,
                            alice_params,
                            bob_parameters,
                            &core_arb_txs,
                            public_offer,
                        )?;
                        let cosigned_arb_cancel = alice.cosign_arbitrating_cancel(
                            &self.seed,
                            alice_params,
                            bob_parameters,
                            &core_arb_txs,
                            public_offer,
                        )?;
                        let refund_proc_signatures = RefundProcedureSignatures::from_bundles(
                            &cosigned_arb_cancel,
                            &signed_adaptor_refund,
                        )?;
                        let refund_proc_signatures =
                            Msg::RefundProcedureSignatures(refund_proc_signatures);

                        senders.send_to(
                            ServiceBus::Ctl,
                            ServiceId::Farcasterd,
                            source,
                            Request::Protocol(refund_proc_signatures),
                        )?
                    }
                    _ => Err(Error::Farcaster("only Wallet::Alice".to_string()))?,
                }
            }
            Request::Protocol(Msg::RefundProcedureSignatures(refund_proc_sigs)) => {
                let swap_id = if let ServiceId::Swap(swap_id) = source {
                    Ok(swap_id)
                } else {
                    Err(Error::Farcaster("Not swapd".to_string()))
                }?;

                match self.wallets.get_mut(&swap_id) {
                    Some(Wallet::Bob(
                        bob,
                        bob_params,
                        public_offer,
                        Some(_funding_tx),
                        Some(alice_params),
                        Some(core_arbitrating_txs),
                    )) => {
                        // *refund_sigs = Some(refund_proc_sigs);
                        let signed_adaptor_buy = bob.sign_adaptor_buy(
                            &self.seed,
                            alice_params,
                            bob_params,
                            core_arbitrating_txs,
                            public_offer,
                        )?;
                        let signed_arb_lock =
                            bob.sign_arbitrating_lock(&self.seed, core_arbitrating_txs)?;

                        // TODO: here subscribe to all transactions with syncerd, and publish lock
                        let buy_proc_sig =
                            BuyProcedureSignature::<BtcXmr>::from_bundle(&signed_adaptor_buy)?;
                        let buy_proc_sig = Msg::BuyProcedureSignature(buy_proc_sig);
                        senders.send_to(
                            ServiceBus::Ctl,
                            ServiceId::Farcasterd,
                            source, // destination swapd
                            Request::Protocol(buy_proc_sig),
                        )?
                    }
                    _ => Err(Error::Farcaster("Unknow wallet and swap_id".to_string()))?,
                }
            }

            Request::GetInfo => {
                senders.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Farcasterd, // source
                    source,                // destination
                    Request::NodeInfo(NodeInfo {
                        node_id: self.node_id,
                        listens: self.listens.iter().cloned().collect(),
                        uptime: SystemTime::now()
                            .duration_since(self.started)
                            .unwrap_or(Duration::from_secs(0)),
                        since: self
                            .started
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap_or(Duration::from_secs(0))
                            .as_secs(),
                        peers: self.connections.iter().cloned().collect(),
                        swaps: self.running_swaps.iter().cloned().collect(),
                        offers: self.making_offers.iter().cloned().collect(),
                    }),
                )?;
            }

            Request::ListPeers => {
                senders.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Farcasterd, // source
                    source,                // destination
                    Request::PeerList(self.connections.iter().cloned().collect()),
                )?;
            }

            Request::ListSwaps => {
                senders.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Farcasterd, // source
                    source,                // destination
                    Request::SwapList(self.running_swaps.iter().cloned().collect()),
                )?;
            }

            Request::Listen(addr) => {
                let addr_str = addr.addr();
                if self.listens.contains(&addr) {
                    let msg = format!("Listener on {} already exists, ignoring request", addr);
                    warn!("{}", msg.err());
                    notify_cli.push((
                        Some(source.clone()),
                        Request::Failure(Failure { code: 1, info: msg }),
                    ));
                } else {
                    self.listens.insert(addr);
                    info!(
                        "{} for incoming LN peer connections on {}",
                        "Starting listener".promo(),
                        addr_str
                    );
                    let resp = self.listen(addr);
                    match resp {
                        Ok(_) => info!(
                            "Connection daemon {} for incoming LN peer connections on {}",
                            "listens".ended(),
                            addr_str
                        ),
                        Err(ref err) => error!("{}", err.err()),
                    }
                    senders.send_to(
                        ServiceBus::Ctl,
                        ServiceId::Farcasterd,
                        source.clone(),
                        resp.into_progress_or_failure(),
                    )?;
                    notify_cli.push((
                        Some(source.clone()),
                        Request::Success(OptionDetails::with(format!(
                            "Node {} listens for connections on {}",
                            self.node_id, addr
                        ))),
                    ));
                }
            }

            Request::ConnectPeer(addr) => {
                info!(
                    "{} to remote peer {}",
                    "Connecting".promo(),
                    addr.promoter()
                );
                let resp = self.connect_peer(source.clone(), addr);
                match resp {
                    Ok(_) => {}
                    Err(ref err) => error!("{}", err.err()),
                }
                notify_cli.push((Some(source.clone()), resp.into_progress_or_failure()));
            }

            // Request::OpenSwapWith(request::CreateSwap {
            //     swap_req,
            //     peerd,
            //     report_to,
            // }) => {
            //     info!(
            //         "{} by request from {}",
            //         "Creating channel".promo(),
            //         source.promoter()
            //     );
            //     let resp = self.create_swap(peerd, report_to, swap_req,
            // false);     match resp {
            //         Ok(_) => {}
            //         Err(ref err) => error!("{}", err.err()),
            //     }
            //     notify_cli.push(Some((
            //         Some(source.clone()),
            //         resp.into_progress_or_failure(),
            //     ));
            // }
            Request::MakeOffer(request::ProtoPublicOffer { offer, remote_addr }) => {
                if !self.listens.contains(&remote_addr) {
                    self.listens.insert(remote_addr);
                    info!(
                        "{} for incoming LN peer connections on {}",
                        "Starting listener".promo(),
                        remote_addr.promo()
                    );
                    let resp = self.listen(remote_addr);
                    match resp {
                        Ok(_) => info!(
                            "Connection daemon {} for incoming LN peer connections on {}",
                            "listens".ended(),
                            remote_addr
                        ),
                        Err(ref err) => error!("{}", err.err()),
                    }
                    notify_cli.push((
                        Some(source.clone()),
                        resp.into_progress_or_failure()
                        // Request::Progress(format!(
                        //     "Node {} listens for connections on {}",
                        //     self.node_id, remote_addr
                        // )),
                    ));
                } else {
                    info!("Already listening on {}", &remote_addr);
                    let msg = format!("Already listening on {}", &remote_addr);
                    notify_cli.push((Some(source.clone()), Request::Progress(msg)));
                }
                let peer = internet2::RemoteNodeAddr {
                    node_id: self.node_id,
                    remote_addr,
                };
                let public_offer = offer.to_public_v1(peer);
                let hex_public_offer = public_offer.to_string();
                if self.making_offers.insert(public_offer) {
                    let msg = format!(
                        "{} {}",
                        "Pubic offer registered, please share with taker: ".promo(),
                        hex_public_offer.amount()
                    );
                    info!(
                        "{} {}",
                        "Pubic offer registered:".promo(),
                        &hex_public_offer.amount()
                    );
                    notify_cli.push((
                        Some(source.clone()),
                        Request::Success(OptionDetails(Some(msg))),
                    ));
                    // senders.send_to(
                    //     ServiceBus::Ctl,
                    //     ServiceId::Farcasterd, // source
                    //     source,                // destination
                    //     Request::PublicOfferHex(hex_public_offer),
                    // )?;
                } else {
                    let msg = "This Public offer was previously registered";
                    warn!("{}", msg.err());
                    notify_cli.push((
                        Some(source.clone()),
                        Request::Failure(Failure {
                            code: 1,
                            info: msg.to_string(),
                        }),
                    ));
                }
            }

            Request::TakeOffer(public_offer) => {
                if self.making_offers.contains(&public_offer) {
                    let msg = format!(
                        "Offer {} already exists, ignoring request",
                        &public_offer.to_string()
                    );
                    warn!("{}", msg.err());
                    notify_cli.push((
                        Some(source.clone()),
                        Request::Failure(Failure { code: 1, info: msg }),
                    ));
                } else {
                    let PublicOffer {
                        version,
                        offer,
                        daemon_service,
                    } = public_offer.clone();
                    let peer = daemon_service
                        .to_node_addr(LIGHTNING_P2P_DEFAULT_PORT)
                        .ok_or_else(|| internet2::presentation::Error::InvalidEndpoint)?;

                    // Connect
                    let peer_connected_is_ok = if !self.connections.contains(&peer) {
                        info!(
                            "{} to remote peer {}",
                            "Connecting".promo(),
                            peer.promoter()
                        );
                        let peer_connected = self.connect_peer(source.clone(), peer.clone());

                        let peer_connected_is_ok = peer_connected.is_ok();

                        notify_cli.push((
                            Some(source.clone()),
                            peer_connected.into_progress_or_failure(),
                        ));
                        peer_connected_is_ok
                    } else {
                        let msg = format!("Already connected to remote peer {}", peer.promoter());
                        warn!("{}", &msg);

                        notify_cli.push((Some(source.clone()), Request::Progress(msg)));
                        true
                    };

                    if peer_connected_is_ok {
                        let offer_registered = format!(
                            "{} {}",
                            "Pubic offer registered:".promo(),
                            &public_offer.amount()
                        );
                        // not yet in the set
                        self.making_offers.insert(public_offer.clone());
                        info!("{}", offer_registered.amount());

                        notify_cli.push((
                            Some(source.clone()),
                            Request::Success(OptionDetails(Some(offer_registered))),
                        ));
                    }

                    let swap_id: SwapId = TempSwapId::random().into(); // TODO: replace by public_offer_id
                                                                       // since we're takers, we are on the other side
                    let taker = NegotiationRole::Taker;
                    let taker_role = offer.maker_role.other();
                    match taker_role {
                        SwapRole::Bob => {
                            let address = bitcoin::Address::from_str(
                                "bc1qesgvtyx9y6lax0x34napc2m7t5zdq6s7xxwpvk",
                            )
                            .expect("Parsable address");
                            let bob: Bob<BtcXmr> = Bob::new(address.into(), FeePolitic::Aggressive);
                            let params =
                                bob.generate_parameters(&self.seed, &self.seed, &public_offer)?;
                            launch_swapd(
                                self,
                                peer.into(),
                                Some(source),
                                taker,
                                public_offer,
                                Params::Bob(params),
                                swap_id,
                            )?;
                        }
                        SwapRole::Alice => {
                            let address = bitcoin::Address::from_str(
                                "bc1qesgvtyx9y6lax0x34napc2m7t5zdq6s7xxwpvk",
                            )
                            .expect("Parsable address");
                            let alice: Alice<BtcXmr> =
                                Alice::new(address.into(), FeePolitic::Aggressive);
                            let params =
                                alice.generate_parameters(&self.seed, &self.seed, &public_offer)?;
                            launch_swapd(
                                self,
                                peer.into(),
                                Some(source),
                                taker,
                                public_offer,
                                Params::Alice(params),
                                swap_id,
                            )?;
                        }
                    };
                }
            }

            Request::Init(_) => {}
            Request::Error(_) => {}
            Request::Ping(_) => {}
            Request::Pong(_) => {}
            Request::PeerMessage(_) => {}
            Request::Protocol(_) => {}
            Request::ListTasks => {}
            Request::PingPeer => {}
            Request::TakeSwap(_) => {}
            Request::FundSwap(_) => {}
            Request::Progress(_) => {}
            Request::Success(_) => {}
            Request::Failure(_) => {}
            Request::SyncerInfo(_) => {}
            Request::NodeInfo(_) => {}
            Request::PeerInfo(_) => {}
            Request::SwapInfo(_) => {}
            Request::TaskList(_) => {}
            // Request::SwapFunding(_) => {}
            Request::CreateTask(_) => {}
            _ => unimplemented!(),
        }
        let mut len = 0;
        for (respond_to, resp) in notify_cli.into_iter() {
            if let Some(respond_to) = respond_to {
                len += 1;
                println!("{}", len);
                info!(
                    "Respond to {} -> Response {}",
                    respond_to.amount(),
                    resp.promo(),
                );
                senders.send_to(ServiceBus::Ctl, ServiceId::Farcasterd, respond_to, resp)?;
            }
        }

        Ok(())
    }
    fn take_offer(
        &mut self,
        source: ServiceId,
        report_to: Option<ServiceId>,
        mut swap_req: message::AcceptChannel,
        accept: bool,
    ) -> Result<(), Error> {
        debug!("Instantiating swapd...");

        // We need to initialize temporary channel id here
        if !accept {
            swap_req.temporary_channel_id = TempSwapId::random();
            debug!(
                "Generated {} as a temporary channel id",
                swap_req.temporary_channel_id
            );
        }

        // Start swapd
        let child = launch("swapd", &[swap_req.temporary_channel_id.to_hex()])?;
        let msg = format!("New instance of swapd launched with PID {}", child.id());
        info!("{}", msg);
        Ok(())
    }
    fn listen(&mut self, addr: RemoteSocketAddr) -> Result<String, Error> {
        if let RemoteSocketAddr::Ftcp(inet) = addr {
            let socket_addr = SocketAddr::try_from(inet)?;
            let ip = socket_addr.ip();
            let port = socket_addr.port();

            debug!("Instantiating peerd...");

            // Start peerd
            let child = launch(
                "peerd",
                &["--listen", &ip.to_string(), "--port", &port.to_string()],
            )?;
            let msg = format!("New instance of peerd launched with PID {}", child.id());
            info!("{}", msg);
            Ok(msg)
        } else {
            Err(Error::Other(s!(
                "Only TCP is supported for now as an overlay protocol"
            )))
        }
    }

    fn connect_peer(&mut self, source: ServiceId, node_addr: NodeAddr) -> Result<String, Error> {
        debug!("Instantiating peerd...");
        if self.connections.contains(&node_addr) {
            return Err(Error::Other(format!(
                "Already connected to peer {}",
                node_addr
            )));
        }
        // Start peerd
        let child = launch("peerd", &["--connect", &node_addr.to_string()]);

        // in case it can't connect wait for it to crash
        std::thread::sleep(Duration::from_secs_f32(0.5));

        // status is Some if peerd returns because it crashed
        let (child, status) = child.and_then(|mut c| c.try_wait().map(|s| (c, s)))?;

        if let Some(_) = status {
            return Err(Error::Peer(internet2::presentation::Error::InvalidEndpoint));
        }

        let msg = format!("New instance of peerd launched with PID {}", child.id());
        info!("{}", msg);

        self.spawning_services
            .insert(ServiceId::Peer(node_addr), source);
        debug!("Awaiting for peerd to connect...");

        Ok(msg)
    }
}

fn launch_swapd(
    runtime: &mut Runtime,
    peerd: ServiceId,
    report_to: Option<ServiceId>,
    negotiation_role: NegotiationRole,
    public_offer: PublicOffer<BtcXmr>,
    params: Params,
    swap_id: SwapId,
) -> Result<String, Error> {
    debug!("Instantiating swapd...");
    let child = launch(
        "swapd",
        &[
            swap_id.to_hex(),
            public_offer.to_string(),
            negotiation_role.to_string(),
        ],
    )?;
    let msg = format!("New instance of swapd launched with PID {}", child.id());
    info!("{}", msg);

    let list = match negotiation_role {
        NegotiationRole::Taker => &mut runtime.taking_swaps,
        NegotiationRole::Maker => &mut runtime.making_swaps,
    };
    list.insert(
        ServiceId::Swap(swap_id),
        request::InitSwap {
            peerd,
            report_to,
            params,
            swap_id,
        },
    );
    debug!("Awaiting for swapd to connect...");

    Ok(msg)
}

fn launch(
    name: &str,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> io::Result<process::Child> {
    let mut bin_path = std::env::current_exe().map_err(|err| {
        error!("Unable to detect binary directory: {}", err);
        err
    })?;
    bin_path.pop();

    bin_path.push(name);
    #[cfg(target_os = "windows")]
    bin_path.set_extension("exe");

    debug!(
        "Launching {} as a separate process using `{}` as binary",
        name,
        bin_path.to_string_lossy()
    );

    let mut cmd = process::Command::new(bin_path);
    cmd.args(std::env::args().skip(1)).args(args);
    trace!("Executing `{:?}`", cmd);
    cmd.spawn().map_err(|err| {
        error!("Error launching {}: {}", name, err);
        err
    })
}
