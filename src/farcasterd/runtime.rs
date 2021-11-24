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

use crate::{
    error::SyncerError,
    rpc::request::{
        BitcoinAddress, Keys, LaunchSwap, MoneroAddress, PubOffer, RequestId, Reveal, Token,
    },
    swapd::get_swap_id,
    syncerd::opts::Coin,
    walletd::NodeSecrets,
    Senders,
};
use amplify::Wrapper;
use request::{Commit, Params};
use std::io;
use std::net::SocketAddr;
use std::process;
use std::time::{Duration, SystemTime};
use std::{collections::VecDeque, hash::Hash};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
};
use std::{convert::TryFrom, thread::sleep};
use std::{convert::TryInto, ffi::OsStr};

use bitcoin::{
    hashes::hex::ToHex,
    secp256k1::{PublicKey, SecretKey},
};
use bitcoin::{
    secp256k1::{
        self,
        rand::{thread_rng, RngCore},
    },
    Address,
};
use internet2::{addr::InetSocketAddr, NodeAddr, RemoteSocketAddr, ToNodeAddr, TypedEnum};
use lnp::{message, Messages, TempChannelId as TempSwapId, LIGHTNING_P2P_DEFAULT_PORT};
use lnpbp::chain::Chain;
use microservices::esb::{self, Handler};
use microservices::rpc::Failure;

use farcaster_core::{blockchain::Network, negotiation::PublicOfferId, swap::SwapId};

use crate::rpc::request::{GetKeys, IntoProgressOrFalure, Msg, NodeInfo, OptionDetails};
use crate::rpc::{request, Request, ServiceBus};
use crate::{Config, Error, LogStyle, Service, ServiceId};

use farcaster_core::{
    blockchain::FeePriority,
    bundle::{
        AliceParameters, BobParameters, CoreArbitratingTransactions, FundingTransaction,
        SignedArbitratingLock,
    },
    negotiation::PublicOffer,
    protocol_message::{
        BuyProcedureSignature, CommitAliceParameters, CommitBobParameters, CoreArbitratingSetup,
        RefundProcedureSignatures,
    },
    role::{Alice, Bob, SwapRole, TradeRole},
    swap::btcxmr::{BtcXmr, KeyManager},
};

use std::str::FromStr;

pub fn run(config: Config, wallet_token: Token) -> Result<(), Error> {
    let _walletd = launch("walletd", &["--wallet-token", &wallet_token.to_string()])?;
    let runtime = Runtime {
        identity: ServiceId::Farcasterd,
        listens: none!(),
        started: SystemTime::now(),
        connections: none!(),
        running_swaps: none!(),
        spawning_services: none!(),
        making_swaps: none!(),
        taking_swaps: none!(),
        arb_addrs: none!(),
        acc_addrs: none!(),
        public_offers: none!(),
        node_ids: none!(),
        wallet_token,
        pending_requests: none!(),
        syncer_services: none!(),
        syncer_clients: none!(),
        consumed_offers: none!(),
        progress: none!(),
        stats: none!(),
    };

    let broker = true;
    Service::run(config, runtime, broker)
}

pub struct Runtime {
    identity: ServiceId,
    listens: HashSet<RemoteSocketAddr>,
    started: SystemTime,
    connections: HashSet<NodeAddr>,
    running_swaps: HashSet<SwapId>,
    spawning_services: HashMap<ServiceId, ServiceId>,
    making_swaps: HashMap<ServiceId, (request::InitSwap, Network)>,
    taking_swaps: HashMap<ServiceId, (request::InitSwap, Network)>,
    public_offers: HashSet<PublicOffer<BtcXmr>>,
    arb_addrs: HashMap<PublicOfferId, bitcoin::Address>,
    acc_addrs: HashMap<PublicOfferId, monero::Address>,
    consumed_offers: HashSet<(PublicOfferId, SwapId)>,
    node_ids: HashSet<PublicKey>, // TODO is it possible? HashMap<SwapId, PublicKey>
    wallet_token: Token,
    pending_requests: HashMap<request::RequestId, (Request, ServiceId)>,
    syncer_services: HashMap<(Coin, Network), ServiceId>,
    syncer_clients: HashMap<(Coin, Network), HashSet<SwapId>>,
    progress: HashMap<ServiceId, VecDeque<Request>>,
    stats: Stats,
}

struct Stats {
    success: u64,
    failure: u64,
}

impl Stats {
    fn incr_success(&mut self) {
        self.success += 1
    }
    fn incr_failure(&mut self) {
        self.failure += 1
    }
    fn success_rate(&self) -> f64 {
        let Stats { success, failure } = self;
        let total = success + failure;
        let rate = *success as f64 / (total as f64);
        info!(
            "Swap success rate: Success({}) / Total({})  = {}",
            success,
            total,
            rate.bright_yellow_bold()
        );
        rate
    }
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            success: 0,
            failure: 0,
        }
    }
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
    fn clean_up_after_swap(&mut self, swapid: &SwapId) {
        self.running_swaps.remove(swapid);
        let offers2rm: Vec<_> = self
            .consumed_offers
            .iter()
            .filter(|(_, i_swapid)| swapid == i_swapid)
            .cloned()
            .collect();

        for offer in &offers2rm {
            self.consumed_offers.remove(offer);
        }

        self.syncer_clients = self
            .syncer_clients
            .drain()
            .map(|(key, mut xs)| {
                xs.remove(swapid);
                (key, xs)
            })
            .collect();

        let syncers_wo_client: Vec<_> = self
            .syncer_clients
            .iter()
            .filter(|(_, xs)| xs.is_empty())
            .map(|(k, _)| k)
            .collect();

        if !syncers_wo_client.is_empty() {
            warn!("Some syncers have no client and may exit");
        }
    }

    fn consumed_offers_contains(&self, offerid: &PublicOfferId) -> bool {
        self.consumed_offers
            .iter()
            .filter(|(i_offerid, _)| i_offerid == offerid)
            .find_map(|_| Some(true))
            .unwrap_or(false)
    }

    fn _send_walletd(&self, senders: &mut Senders, message: request::Request) -> Result<(), Error> {
        senders.send_to(ServiceBus::Ctl, self.identity(), ServiceId::Wallet, message)?;
        Ok(())
    }
    fn node_ids(&self) -> Vec<PublicKey> {
        self.node_ids.iter().cloned().collect()
    }

    fn _known_swap_id(&self, source: ServiceId) -> Result<SwapId, Error> {
        let swap_id = get_swap_id(&source)?;
        if self.running_swaps.contains(&swap_id) {
            Ok(swap_id)
        } else {
            Err(Error::Farcaster("Unknown swapd".to_string()))
        }
    }
    fn handle_rpc_msg(
        &mut self,
        senders: &mut esb::SenderList<ServiceBus, ServiceId>,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        match &request {
            Request::Hello => {
                // Ignoring; this is used to set remote identity at ZMQ level
            }

            // 1st protocol message received through peer connection, and last
            // handled by farcasterd, receiving taker commit because we are
            // maker
            Request::Protocol(Msg::TakerCommit(request::TakeCommit {
                commit: _,
                public_offer,
                swap_id,
            })) => {
                let public_offer: PublicOffer<BtcXmr> = FromStr::from_str(public_offer)?;
                // public offer gets removed on LaunchSwap
                if !self.public_offers.contains(&public_offer) {
                    warn!(
                        "Unknow offer {}, you are not the maker of that offer, ignoring it",
                        &public_offer
                    );
                } else {
                    trace!(
                        "Offer {} is known, you created it previously, engaging walletd to initiate swap with taker",
                        &public_offer
                    );
                    if let Some(arb_addr) = self.arb_addrs.remove(&public_offer.id()) {
                        let btc_addr_req =
                            Request::BitcoinAddress(BitcoinAddress(*swap_id, arb_addr));
                        senders.send_to(
                            ServiceBus::Msg,
                            self.identity(),
                            ServiceId::Wallet,
                            btc_addr_req,
                        )?;
                    } else {
                        error!("missing arb_addr")
                    }
                    if let Some(acc_addr) = self.acc_addrs.remove(&public_offer.id()) {
                        let xmr_addr_req =
                            Request::MoneroAddress(MoneroAddress(*swap_id, acc_addr));
                        senders.send_to(
                            ServiceBus::Msg,
                            self.identity(),
                            ServiceId::Wallet,
                            xmr_addr_req,
                        )?;
                    } else {
                        error!("missing acc_addr")
                    }

                    senders.send_to(ServiceBus::Msg, source, ServiceId::Wallet, request)?;
                }
                return Ok(());
            }
            _ => {
                error!("MSG RPC can be only used for forwarding farcaster protocol messages");
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
        let mut report_to: Vec<(Option<ServiceId>, Request)> = none!();
        match request.clone() {
            Request::Hello => {
                // Ignoring; this is used to set remote identity at ZMQ level
                info!(
                    "{} daemon is {}",
                    source.bright_green_bold(),
                    "connected".bright_green_bold()
                );

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
                        if self.running_swaps.insert(*swap_id) {
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
                    ServiceId::Syncer(coin, network)
                        if !self.syncer_services.contains_key(&(coin.clone(), *network)) =>
                    {
                        self.syncer_services
                            .insert((coin.clone(), *network), source.clone());
                    }
                    _ => {
                        // Ignoring the rest of daemon/client types
                    }
                };

                if let Some((swap_params, network)) = self.making_swaps.get(&source) {
                    // Tell swapd swap options and link it with the
                    // connection daemon
                    debug!(
                        "Swapd {} is known: we spawned it to create a swap. \
                         Requesting swapd to be the maker of this swap",
                        source
                    );
                    report_to.push((
                        swap_params.report_to.clone(), // walletd
                        Request::Progress(format!("Swap daemon {} operational", source)),
                    ));
                    let swapid = get_swap_id(&source)?;
                    // when online, Syncers say Hello, then they get registered to self.syncers
                    syncers_up(
                        &self.syncer_services,
                        &mut self.syncer_clients,
                        Coin::Bitcoin,
                        *network,
                        swapid,
                    )?;
                    syncers_up(
                        &self.syncer_services,
                        &mut self.syncer_clients,
                        Coin::Monero,
                        *network,
                        swapid,
                    )?;
                    // FIXME msgs should go to walletd?
                    senders.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        source.clone(),
                        Request::MakeSwap(swap_params.clone()),
                    )?;
                    self.running_swaps.insert(swap_params.swap_id);
                    self.making_swaps.remove(&source);
                } else if let Some((swap_params, network)) = self.taking_swaps.get(&source) {
                    // Tell swapd swap options and link it with the
                    // connection daemon
                    debug!(
                        "Daemon {} is known: we spawned it to create a swap. \
                         Requesting swapd to be the taker of this swap",
                        source
                    );
                    report_to.push((
                        swap_params.report_to.clone(), // walletd
                        Request::Progress(format!("Swap daemon {} operational", source)),
                    ));
                    match swap_params.local_params {
                        Params::Alice(_) => {}
                        Params::Bob(_) => {}
                    }

                    let swapid = get_swap_id(&source)?;
                    syncers_up(
                        &self.syncer_services,
                        &mut self.syncer_clients,
                        Coin::Bitcoin,
                        *network,
                        swapid,
                    )?;
                    syncers_up(
                        &self.syncer_services,
                        &mut self.syncer_clients,
                        Coin::Monero,
                        *network,
                        swapid,
                    )?;
                    // FIXME msgs should go to walletd?
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
                    report_to.push((
                        Some(enquirer.clone()),
                        Request::Success(OptionDetails::with(format!(
                            "Peer connected to {}",
                            source
                        ))),
                    ));
                    self.spawning_services.remove(&source);
                }
            }
            Request::SwapSuccess(success) => {
                let swapid = get_swap_id(&source)?;
                self.clean_up_after_swap(&swapid);
                if success {
                    info!("Success on swap {}", &swapid);
                    self.stats.incr_success();
                } else {
                    info!("Failure on swap {}", &swapid);
                    self.stats.incr_failure();
                }
                self.stats.success_rate();
                senders.send_to(ServiceBus::Ctl, self.identity(), source, request)?;
            }
            Request::LaunchSwap(LaunchSwap {
                maker_node_id,
                local_trade_role,
                public_offer,
                local_params,
                swap_id,
                remote_commit,
                funding_address,
            }) => {
                let (node_id, peer_address) = match (local_trade_role, self.listens.len()) {
                    // Maker has only one listener, MAYBE for more listeners self.listens may be a
                    // HashMap<RemoteSocketAddr, Vec<OfferId>>
                    (TradeRole::Maker, 1) => (
                        maker_node_id,
                        self.listens
                            .clone()
                            .into_iter()
                            .find_map(Some)
                            .expect("exactly 1 listener checked on pattern match"),
                    ),
                    (TradeRole::Taker, _) if public_offer.node_id == maker_node_id => (
                        public_offer.node_id,
                        internet2::RemoteSocketAddr::Ftcp(public_offer.peer_address),
                    ),
                    _ => {
                        error!("Currently only one listener supported!");
                        return Ok(());
                    }
                };
                if self.public_offers.remove(&public_offer) {
                    trace!(
                        "{}, {}",
                        "launching swapd with swap_id:",
                        swap_id.bright_yellow_bold()
                    );
                    let daemon_service = internet2::RemoteNodeAddr {
                        node_id,
                        remote_addr: peer_address,
                    };
                    let peer = daemon_service
                        .to_node_addr(internet2::LIGHTNING_P2P_DEFAULT_PORT)
                        .ok_or(internet2::presentation::Error::InvalidEndpoint)?
                        .into();

                    self.consumed_offers
                        .insert((public_offer.id(), swap_id.clone()));
                    launch_swapd(
                        self,
                        peer,
                        Some(self.identity()),
                        local_trade_role,
                        public_offer,
                        local_params,
                        swap_id,
                        remote_commit,
                        funding_address,
                    )?;
                } else {
                    let msg = "unknown public_offer".to_string();
                    error!("{}", msg);
                    return Err(Error::Farcaster(msg));
                }
            }
            Request::Keys(Keys(sk, pk, id)) if self.pending_requests.contains_key(&id) => {
                trace!("received peerd keys");
                if let Some((request, source)) = self.pending_requests.remove(&id) {
                    // storing node_id
                    self.node_ids.insert(pk);
                    trace!("Received expected peer keys, injecting key in request");
                    let req = if let Request::MakeOffer(mut req) = request {
                        req.peer_secret_key = Some(sk);
                        Ok(Request::MakeOffer(req))
                    } else if let Request::TakeOffer(mut req) = request {
                        req.peer_secret_key = Some(sk);
                        Ok(Request::TakeOffer(req))
                    } else {
                        Err(Error::Farcaster(s!(
                            "Unexpected request: calling back from Keypair handling"
                        )))
                    }?;
                    trace!("Procede executing pending request");
                    // recurse with request containing key
                    self.handle_rpc_ctl(senders, source, req)?
                } else {
                    error!("Received unexpected peer keys");
                }
            }
            Request::GetInfo => {
                senders.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Farcasterd, // source
                    source,                // destination
                    Request::NodeInfo(NodeInfo {
                        node_ids: self.node_ids(),
                        listens: self.listens.iter().cloned().collect(),
                        uptime: SystemTime::now()
                            .duration_since(self.started)
                            .unwrap_or_else(|_| Duration::from_secs(0)),
                        since: self
                            .started
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap_or_else(|_| Duration::from_secs(0))
                            .as_secs(),
                        peers: self.connections.iter().cloned().collect(),
                        swaps: self.running_swaps.iter().cloned().collect(),
                        offers: self.public_offers.iter().cloned().collect(),
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

            // Request::Listen(addr) => {
            //     let addr_str = addr.addr();
            //     if self.listens.contains(&addr) {
            //         let msg = format!("Listener on {} already exists, ignoring request", addr);
            //         warn!("{}", msg.err());
            //         report_to.push((
            //             Some(source.clone()),
            //             Request::Failure(Failure { code: 1, info: msg }),
            //         ));
            //     } else {
            //         let (_, sk) = self.peerd_keys()?;
            //         let resp = self.listen(&addr, sk);
            //         self.listens.insert(addr);
            //         info!(
            //             "{} for incoming LN peer connections on {}",
            //             "Starting listener".bright_blue_bold(),
            //             addr_str
            //         );
            //         match resp {
            //             Ok(_) => info!(
            //                 "Connection daemon {} for incoming LN peer connections on {}",
            //                 "listens".bright_green_bold(),
            //                 addr_str
            //             ),
            //             Err(ref err) => error!("{}", err.err()),
            //         }

            //         senders.send_to(
            //             ServiceBus::Ctl,
            //             ServiceId::Farcasterd,
            //             source.clone(),
            //             resp.into_progress_or_failure(),
            //         )?;
            //         report_to.push((
            //             Some(source.clone()),
            //             Request::Success(OptionDetails::with(format!(
            //                 "Node {} listens for connections on {}",
            //                 self.node_ids()[0], // FIXME
            //                 addr
            //             ))),
            //         ));
            //     }
            // }

            // Request::ConnectPeer(addr) => {
            //     info!(
            //         "{} to remote peer {}",
            //         "Connecting".bright_blue_bold(),
            //         addr.bright_blue_italic()
            //     );
            //     let resp = self.connect_peer(source.clone(), &addr);
            //     match resp {
            //         Ok(_) => {}
            //         Err(ref err) => error!("{}", err.err()),
            //     }
            //     report_to.push((Some(source.clone()), resp.into_progress_or_failure()));
            // }

            // Request::OpenSwapWith(request::CreateSwap {
            //     swap_req,
            //     peerd,
            //     report_to,
            // }) => {
            //     info!(
            //         "{} by request from {}",
            //         "Creating channel".bright_blue_bold(),
            //         source.bright_blue_italic()
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
            Request::MakeOffer(request::ProtoPublicOffer {
                offer,
                public_addr,
                bind_addr,
                peer_secret_key,
                arbitrating_addr,
                accordant_addr,
            }) => {
                let resp = match (self.listens.contains(&bind_addr), peer_secret_key) {
                    (false, None) => {
                        trace!("Push MakeOffer to pending_requests and requesting a secret from Wallet");
                        return self.get_secret(senders, source, request);
                    }
                    (false, Some(sk)) => {
                        self.listens.insert(bind_addr);
                        info!(
                            "{} for incoming LN peer connections on {}",
                            "Starting listener".bright_blue_bold(),
                            bind_addr.bright_blue_bold()
                        );
                        self.listen(&bind_addr, sk)
                    }
                    (true, _) => {
                        let msg = format!("Already listening on {}", &bind_addr);
                        info!("{}", &msg);
                        Ok(msg)
                    }
                };
                match resp {
                    Ok(_) => info!(
                        "Connection daemon {} for incoming LN peer connections on {}",
                        "listens".bright_green_bold(),
                        bind_addr
                    ),
                    Err(err) => {
                        error!("{}", err.err());
                        return Err(err);
                    }
                }
                report_to.push((
                        Some(source.clone()),
                        resp.into_progress_or_failure()
                        // Request::Progress(format!(
                        //     "Node {} listens for connections on {}",
                        //     self.node_id, remote_addr
                        // )),
                    ));

                let node_ids = self.node_ids();
                if node_ids.len() != 1 {
                    error!("{}", "Currently node supports only 1 node id");
                    return Ok(());
                }

                let public_offer = offer.to_public_v1(node_ids[0], public_addr.into());
                let pub_offer_id = public_offer.id();
                let serialized_offer = public_offer.to_string();
                if self.public_offers.insert(public_offer) {
                    let msg = format!(
                        "{} {}",
                        "Public offer registered, please share with taker: ".bright_blue_bold(),
                        serialized_offer.bright_yellow_bold()
                    );
                    info!(
                        "Maker: {} {}",
                        "Public offer registered".bright_blue_bold(),
                        pub_offer_id.bright_yellow_bold()
                    );
                    report_to.push((
                        Some(source.clone()),
                        Request::Success(OptionDetails(Some(msg))),
                    ));
                    self.arb_addrs.insert(pub_offer_id, arbitrating_addr);
                    self.acc_addrs
                        .insert(pub_offer_id, monero::Address::from_str(&accordant_addr)?);
                } else {
                    let msg = "This Public offer was previously registered";
                    warn!("{}", msg.err());
                    report_to.push((
                        Some(source.clone()),
                        Request::Failure(Failure {
                            code: 1,
                            info: msg.to_string(),
                        }),
                    ));
                }
            }

            Request::TakeOffer(request::PubOffer {
                public_offer,
                external_address,
                internal_address,
                peer_secret_key,
            }) => {
                if self.public_offers.contains(&public_offer)
                    || self.consumed_offers_contains(&public_offer.id())
                {
                    let msg = format!(
                        "{} already exists or was already taken, ignoring request",
                        &public_offer.to_string()
                    );
                    warn!("{}", msg.err());
                    report_to.push((
                        Some(source.clone()),
                        Request::Failure(Failure { code: 1, info: msg }),
                    ));
                } else {
                    let PublicOffer {
                        version: _,
                        offer: _,
                        node_id,      // bitcoin::Pubkey
                        peer_address, // InetSocketAddr
                    } = public_offer;

                    let daemon_service = internet2::RemoteNodeAddr {
                        node_id,                                           // checked above
                        remote_addr: RemoteSocketAddr::Ftcp(peer_address), /* expected RemoteSocketAddr */
                    };
                    let peer = daemon_service
                        .to_node_addr(LIGHTNING_P2P_DEFAULT_PORT)
                        .ok_or(internet2::presentation::Error::InvalidEndpoint)?;

                    // Connect
                    let peer_connected_is_ok =
                        match (self.connections.contains(&peer), peer_secret_key) {
                            (false, None) => return self.get_secret(senders, source, request),
                            (false, Some(sk)) => {
                                trace!(
                                    "{} to remote peer {}",
                                    "Connecting".bright_blue_bold(),
                                    peer.bright_blue_italic()
                                );
                                let peer_connected = self.connect_peer(source.clone(), &peer, sk);

                                let peer_connected_is_ok = peer_connected.is_ok();

                                report_to.push((
                                    Some(source.clone()),
                                    peer_connected.into_progress_or_failure(),
                                ));
                                peer_connected_is_ok
                            }
                            (true, _) => {
                                let msg = format!(
                                    "Already connected to remote peer {}",
                                    peer.bright_blue_italic()
                                );

                                warn!("{}", &msg);

                                report_to.push((Some(source.clone()), Request::Progress(msg)));
                                true
                            }
                        };

                    if peer_connected_is_ok {
                        let offer_registered = format!(
                            "{} {}",
                            "Public offer registered:".bright_blue_bold(),
                            &public_offer.id().bright_white_bold()
                        );
                        // not yet in the set
                        self.public_offers.insert(public_offer.clone());
                        info!("{}", offer_registered);
                        let progress = (
                            Some(source.clone()),
                            Request::Success(OptionDetails(Some(offer_registered))),
                        );
                        report_to.push(progress);
                        // reconstruct original request, by drop peer_secret_key
                        // from offer
                        let request = Request::TakeOffer(PubOffer {
                            public_offer,
                            external_address,
                            internal_address,
                            peer_secret_key: None,
                        });
                        senders.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            ServiceId::Wallet,
                            request,
                        )?;
                    }
                }
            }
            Request::Progress(..) | Request::Success(..) | Request::Failure(..) => {
                if !self.progress.contains_key(&source) {
                    self.progress.insert(source.clone(), none!());
                };
                let queue = self.progress.get_mut(&source).expect("checked/added above");
                queue.push_back(request);
            }
            Request::ReadProgress(swapid) => {
                let id = &ServiceId::Swap(swapid);
                if let Some(queue) = self.progress.get_mut(id) {
                    let n = queue.len();

                    for (i, req) in queue.iter().enumerate() {
                        let x = match req {
                            Request::Progress(x)
                            | Request::Success(OptionDetails(Some(x)))
                            | Request::Failure(Failure { code: _, info: x }) => x,
                            _ => unreachable!("not handled here"),
                        };
                        let req = if i < n - 1 {
                            Request::Progress(x.clone())
                        } else {
                            Request::Success(OptionDetails(Some(x.clone())))
                        };
                        report_to.push((Some(source.clone()), req));
                        // senders.send_to(ServiceBus::Ctl, identify.clone(),
                        // source.clone(), req)?;
                    }
                }
            }

            req => {
                error!("Ignoring unsupported request: {}", req.err());
            }
        }

        let mut len = 0;
        for (respond_to, resp) in report_to.into_iter() {
            if let Some(respond_to) = respond_to {
                len += 1;
                debug!("notifications to cli: {}", len);
                trace!(
                    "Respond to {} -> Response {}",
                    respond_to.bright_yellow_bold(),
                    resp.bright_blue_bold(),
                );
                senders.send_to(ServiceBus::Ctl, self.identity(), respond_to, resp)?;
            }
        }
        debug!("processed all cli notifications");
        Ok(())
    }

    fn listen(&mut self, addr: &RemoteSocketAddr, sk: SecretKey) -> Result<String, Error> {
        if let RemoteSocketAddr::Ftcp(inet) = *addr {
            let socket_addr = SocketAddr::try_from(inet)?;
            let ip = socket_addr.ip();
            let port = socket_addr.port();

            debug!("Instantiating peerd...");
            let child = launch(
                "peerd",
                &[
                    "--listen",
                    &ip.to_string(),
                    "--port",
                    &port.to_string(),
                    "--peer-secret-key",
                    &format!("{:x}", sk),
                    "--wallet-token",
                    &self.wallet_token.clone().to_string(),
                ],
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

    fn connect_peer(
        &mut self,
        source: ServiceId,
        node_addr: &NodeAddr,
        sk: SecretKey,
    ) -> Result<String, Error> {
        debug!("Instantiating peerd...");
        if self.connections.contains(node_addr) {
            return Err(Error::Other(format!(
                "Already connected to peer {}",
                node_addr
            )));
        }

        // Start peerd
        let child = launch(
            "peerd",
            &[
                "--connect",
                &node_addr.to_string(),
                "--peer-secret-key",
                &format!("{:x}", sk),
                "--wallet-token",
                &format!("{}", self.wallet_token.clone()),
            ],
        );

        // in case it can't connect wait for it to crash
        std::thread::sleep(Duration::from_secs_f32(0.5));

        // status is Some if peerd returns because it crashed
        let (child, status) = child.and_then(|mut c| c.try_wait().map(|s| (c, s)))?;

        if status.is_some() {
            return Err(Error::Peer(internet2::presentation::Error::InvalidEndpoint));
        }

        let msg = format!("New instance of peerd launched with PID {}", child.id());
        info!("{}", msg);

        self.spawning_services
            .insert(ServiceId::Peer(node_addr.clone()), source);
        debug!("Awaiting for peerd to connect...");

        Ok(msg)
    }

    fn get_secret(
        &mut self,
        senders: &mut esb::SenderList<ServiceBus, ServiceId>,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        trace!(
            "Peer keys not available yet - waiting to receive them on Request::Keypair\
                and then proceed with parent request"
        );
        let req_id = RequestId::rand();
        self.pending_requests
            .insert(req_id.clone(), (request, source));
        let wallet_token = GetKeys(self.wallet_token.clone(), req_id);
        senders.send_to(
            ServiceBus::Ctl,
            ServiceId::Farcasterd,
            ServiceId::Wallet,
            Request::GetKeys(wallet_token),
        )?;
        Ok(())
    }
}

fn syncers_up(
    services: &HashMap<(Coin, Network), ServiceId>,
    clients: &mut HashMap<(Coin, Network), HashSet<SwapId>>,
    coin: Coin,
    network: Network,
    swap_id: SwapId,
) -> Result<(), Error> {
    let k = (coin.clone(), network);
    if !services.contains_key(&k) {
        launch("syncerd", &[coin.to_string(), network.to_string()])?;
        clients.insert(k.clone(), none!());
    }
    if let Some(xs) = clients.get_mut(&k) {
        xs.insert(swap_id);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn launch_swapd(
    runtime: &mut Runtime,
    peerd: ServiceId,
    report_to: Option<ServiceId>,
    local_trade_role: TradeRole,
    public_offer: PublicOffer<BtcXmr>,
    local_params: Params,
    swap_id: SwapId,
    remote_commit: Option<Commit>,
    funding_address: Option<bitcoin::Address>,
) -> Result<String, Error> {
    debug!("Instantiating swapd...");
    let child = launch(
        "swapd",
        &[
            swap_id.to_hex(),
            public_offer.to_string(),
            local_trade_role.to_string(),
        ],
    )?;
    let msg = format!("New instance of swapd launched with PID {}", child.id());
    info!("{}", msg);

    let list = match local_trade_role {
        TradeRole::Taker => &mut runtime.taking_swaps,
        TradeRole::Maker => &mut runtime.making_swaps,
    };
    list.insert(
        ServiceId::Swap(swap_id),
        (
            request::InitSwap {
                peerd,
                report_to,
                local_params,
                swap_id,
                remote_commit,
                funding_address,
            },
            public_offer.offer.network,
        ),
    );

    debug!("Awaiting for swapd to connect...");

    Ok(msg)
}

pub fn launch(
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
