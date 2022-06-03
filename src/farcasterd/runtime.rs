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
    clap::Parser,
    error::SyncerError,
    rpc::request::{
        BitcoinAddress, BitcoinFundingInfo, FundingInfo, Keys, LaunchSwap, MoneroAddress,
        MoneroFundingInfo, Outcome, PubOffer, RequestId, Reveal, Token,
    },
    service::Endpoints,
    swapd::get_swap_id,
    syncerd::opts::Coin,
    walletd::NodeSecrets,
};
use amplify::Wrapper;
use clap::IntoApp;
use request::{Commit, List, Params};
use std::io;
use std::iter::FromIterator;
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
use internet2::{
    addr::InetSocketAddr, NodeAddr, RemoteNodeAddr, RemoteSocketAddr, ToNodeAddr, TypedEnum,
    UrlString,
};
use lnpbp::chain::Chain;
use microservices::esb::{self, Handler};
use microservices::rpc::Failure;

use farcaster_core::{
    blockchain::Network,
    negotiation::{OfferId, PublicOfferId},
    swap::SwapId,
};

use crate::farcasterd::Opts;
use crate::rpc::request::{GetKeys, IntoProgressOrFailure, Msg, NodeInfo, OptionDetails};
use crate::rpc::{request, Request, ServiceBus};
use crate::{Config, Error, LogStyle, Service, ServiceConfig, ServiceId};

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

pub fn run(
    service_config: ServiceConfig,
    config: Config,
    _opts: Opts,
    wallet_token: Token,
) -> Result<(), Error> {
    let _walletd = launch("walletd", &["--token", &wallet_token.to_string()])?;

    if config.is_auto_funding_enable() {
        info!("farcasterd will attempt to fund automatically");
    }

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
        peerd_ids: none!(),
        wallet_token,
        pending_requests: none!(),
        syncer_services: none!(),
        syncer_clients: none!(),
        consumed_offers: none!(),
        progress: none!(),
        stats: none!(),
        funding_xmr: none!(),
        funding_btc: none!(),
        config,
    };

    let broker = true;
    Service::run(service_config, runtime, broker)
}

pub struct Runtime {
    identity: ServiceId,
    listens: HashMap<OfferId, RemoteSocketAddr>,
    started: SystemTime,
    connections: HashSet<NodeAddr>,
    running_swaps: HashSet<SwapId>,
    spawning_services: HashMap<ServiceId, ServiceId>,
    making_swaps: HashMap<ServiceId, (request::InitSwap, Network)>,
    taking_swaps: HashMap<ServiceId, (request::InitSwap, Network)>,
    public_offers: HashSet<PublicOffer<BtcXmr>>,
    arb_addrs: HashMap<PublicOfferId, bitcoin::Address>,
    acc_addrs: HashMap<PublicOfferId, monero::Address>,
    consumed_offers: HashMap<OfferId, SwapId>,
    node_ids: HashMap<OfferId, PublicKey>, // TODO is it possible? HashMap<SwapId, PublicKey>
    peerd_ids: HashMap<OfferId, ServiceId>,
    wallet_token: Token,
    pending_requests: HashMap<request::RequestId, (Request, ServiceId)>,
    syncer_services: HashMap<(Coin, Network), ServiceId>,
    syncer_clients: HashMap<(Coin, Network), HashSet<SwapId>>,
    progress: HashMap<ServiceId, VecDeque<Request>>,
    funding_btc: HashMap<SwapId, (bitcoin::Address, bitcoin::Amount, bool)>,
    funding_xmr: HashMap<SwapId, (monero::Address, monero::Amount, bool)>,
    stats: Stats,
    config: Config,
}

#[derive(Default)]
struct Stats {
    success: u64,
    refund: u64,
    punish: u64,
    initialized: u64,
    awaiting_funding_btc: u64,
    awaiting_funding_xmr: u64,
    funded_xmr: u64,
    funded_btc: u64,
    funding_canceled_xmr: u64,
}

impl Stats {
    fn incr_outcome(&mut self, outcome: &Outcome) {
        match outcome {
            Outcome::Buy => self.success += 1,
            Outcome::Refund => self.refund += 1,
            Outcome::Punish => self.punish += 1,
        };
    }
    fn incr_initiated(&mut self) {
        self.initialized += 1;
    }
    fn incr_awaiting_funding(&mut self, coin: &Coin) {
        match coin {
            Coin::Monero => self.awaiting_funding_xmr += 1,
            Coin::Bitcoin => self.awaiting_funding_btc += 1,
        }
    }
    fn incr_funded(&mut self, coin: &Coin) {
        match coin {
            Coin::Monero => {
                self.funded_xmr += 1;
                self.awaiting_funding_xmr -= 1;
            }
            Coin::Bitcoin => {
                self.funded_btc += 1;
                self.awaiting_funding_btc -= 1;
            }
        }
    }
    fn incr_funding_monero_canceled(&mut self) {
        self.awaiting_funding_xmr -= 1;
        self.funding_canceled_xmr += 1;
    }
    fn success_rate(&self) -> f64 {
        let Stats {
            success,
            refund,
            punish,
            initialized,
            awaiting_funding_btc,
            awaiting_funding_xmr,
            funded_btc,
            funded_xmr,
            funding_canceled_xmr,
        } = self;
        let total = success + refund + punish;
        let rate = *success as f64 / (total as f64);
        info!(
            "Swapped({}) | Refunded({}) / Punished({}) | Initialized({}) / AwaitingFundingXMR({}) / AwaitingFundingBTC({}) / FundedXMR({}) / FundedBTC({}) / FundingCanceledXMR({}) ",
            success.bright_white_bold(),
            refund.bright_white_bold(),
            punish.bright_white_bold(),
            initialized,
            awaiting_funding_xmr.bright_white_bold(),
            awaiting_funding_btc.bright_white_bold(),
            funded_xmr.bright_white_bold(),
            funded_btc.bright_white_bold(),
            funding_canceled_xmr.bright_white_bold(),
        );
        info!(
            "{} = {:>4.3}%",
            "Swap success".bright_blue_bold(),
            (rate * 100.).bright_yellow_bold(),
        );
        rate
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
            ServiceBus::Msg => self.handle_rpc_msg(endpoints, source, request),
            ServiceBus::Ctl => self.handle_rpc_ctl(endpoints, source, request),
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
    fn clean_up_after_swap(
        &mut self,
        swapid: &SwapId,
        endpoints: &mut Endpoints,
    ) -> Result<(), Error> {
        if self.running_swaps.remove(swapid) {
            endpoints.send_to(
                ServiceBus::Ctl,
                self.identity(),
                ServiceId::Swap(*swapid),
                Request::Terminate,
            )?;
        }
        let mut offerid = None;
        self.consumed_offers = self
            .consumed_offers
            .drain()
            .filter_map(|(k, v)| {
                if swapid != &v {
                    Some((k, v))
                } else {
                    offerid = Some(k);
                    None
                }
            })
            .collect();
        let identity = self.identity();
        if let Some(offerid) = &offerid {
            if self.listens.contains_key(offerid) && self.node_ids.contains_key(offerid) {
                self.peerd_ids.remove(offerid);
                let node_id = self.node_ids.remove(offerid).unwrap();
                let remote_addr = self.listens.remove(offerid).unwrap();
                // nr of offers using that peerd
                let peerd_users: Vec<_> = self
                    .listens
                    .values()
                    .filter(|x| x == &&remote_addr)
                    .into_iter()
                    .collect();
                if peerd_users.len() == 0 {
                    let connectionid = NodeAddr::Remote(RemoteNodeAddr {
                        node_id,
                        remote_addr,
                    });

                    if self.connections.remove(&connectionid) {
                        endpoints.send_to(
                            ServiceBus::Ctl,
                            identity.clone(),
                            ServiceId::Peer(connectionid),
                            Request::Terminate,
                        )?;
                    }
                }
            }
        }

        self.syncer_clients = self
            .syncer_clients
            .drain()
            .filter_map(|((coin, network), mut xs)| {
                xs.remove(swapid);
                if !xs.is_empty() {
                    Some(((coin, network), xs))
                } else {
                    let service_id = ServiceId::Syncer(coin, network);
                    info!("Terminating {}", service_id);
                    if endpoints
                        .send_to(
                            ServiceBus::Ctl,
                            identity.clone(),
                            service_id,
                            Request::Terminate,
                        )
                        .is_ok()
                    {
                        None
                    } else {
                        Some(((coin, network), xs))
                    }
                }
            })
            .collect();
        let clients = &self.syncer_clients;
        self.syncer_services = self
            .syncer_services
            .drain()
            .filter(|(k, _)| clients.contains_key(k))
            .collect();
        // self.connections.into_iter().filter(|(o, r)| )

        Ok(())
    }

    fn consumed_offers_contains(&self, offerid: &OfferId) -> bool {
        self.consumed_offers.contains_key(offerid)
    }

    fn _send_walletd(
        &self,
        endpoints: &mut Endpoints,
        message: request::Request,
    ) -> Result<(), Error> {
        endpoints.send_to(ServiceBus::Ctl, self.identity(), ServiceId::Wallet, message)?;
        Ok(())
    }
    fn node_ids(&self) -> Vec<PublicKey> {
        self.node_ids
            .values()
            .into_iter()
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
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
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        match (&request, &source) {
            (Request::Hello, _) => {
                // Ignoring; this is used to set remote identity at ZMQ level
            }

            // 1st protocol message received through peer connection, and last
            // handled by farcasterd, receiving taker commit because we are
            // maker
            (
                Request::Protocol(Msg::TakerCommit(request::TakeCommit {
                    commit: _,
                    public_offer,
                    swap_id,
                })),
                ServiceId::Peer(_),
            ) => {
                let public_offer: PublicOffer<BtcXmr> = FromStr::from_str(public_offer)?;
                // public offer gets removed on LaunchSwap
                if !self.public_offers.contains(&public_offer) {
                    warn!(
                        "Unknown (or already taken) offer {}, you are not the maker of that offer (or you already had a taker for it), ignoring it",
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
                        endpoints.send_to(
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
                        endpoints.send_to(
                            ServiceBus::Msg,
                            self.identity(),
                            ServiceId::Wallet,
                            xmr_addr_req,
                        )?;
                    } else {
                        error!("missing acc_addr")
                    }
                    info!("passing request to walletd from {}", source);
                    self.peerd_ids
                        .insert(public_offer.offer.id(), source.clone());

                    endpoints.send_to(ServiceBus::Msg, source, ServiceId::Wallet, request)?;
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
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        let mut report_to: Vec<(Option<ServiceId>, Request)> = none!();
        match request.clone() {
            Request::Hello => {
                // Ignoring; this is used to set remote identity at ZMQ level
                info!(
                    "Service {} is now {}",
                    source.bright_white_bold(),
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
                                "Connection {} is registered; total {} connections are known",
                                connection_id.bright_blue_italic(),
                                self.connections.len().bright_blue_bold()
                            );
                        } else {
                            warn!(
                                "Connection {} was already registered; the service probably was relaunched",
                                connection_id.bright_blue_italic()
                            );
                        }
                    }
                    ServiceId::Swap(swap_id) => {
                        if self.running_swaps.insert(*swap_id) {
                            info!(
                                "Swap {} is registered; total {} swaps are known",
                                swap_id.bright_blue_italic(),
                                self.running_swaps.len().bright_blue_bold()
                            );
                        } else {
                            warn!(
                                "Swap {} was already registered; the service probably was relaunched",
                                swap_id
                            );
                        }
                    }
                    ServiceId::Syncer(coin, network)
                        if !self.syncer_services.contains_key(&(*coin, *network))
                            && self.spawning_services.contains_key(&source) =>
                    {
                        self.syncer_services
                            .insert((*coin, *network), source.clone());
                        info!(
                            "Syncer {} is registered; total {} syncers are known",
                            &source,
                            self.syncer_services.len().bright_blue_bold()
                        );
                    }
                    ServiceId::Syncer(..) => {
                        error!(
                            "Syncer {} was already registered; the service probably was relaunched\\
                             externally, or maybe multiple syncers launched?",
                            source
                        );
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
                        ServiceId::Farcasterd,
                        &mut self.spawning_services,
                        &self.syncer_services,
                        &mut self.syncer_clients,
                        Coin::Bitcoin,
                        *network,
                        swapid,
                        &self.config,
                    )?;
                    syncers_up(
                        ServiceId::Farcasterd,
                        &mut self.spawning_services,
                        &self.syncer_services,
                        &mut self.syncer_clients,
                        Coin::Monero,
                        *network,
                        swapid,
                        &self.config,
                    )?;
                    // FIXME msgs should go to walletd?
                    endpoints.send_to(
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
                        ServiceId::Farcasterd,
                        &mut self.spawning_services,
                        &self.syncer_services,
                        &mut self.syncer_clients,
                        Coin::Bitcoin,
                        *network,
                        swapid,
                        &self.config,
                    )?;
                    syncers_up(
                        ServiceId::Farcasterd,
                        &mut self.spawning_services,
                        &self.syncer_services,
                        &mut self.syncer_clients,
                        Coin::Monero,
                        *network,
                        swapid,
                        &self.config,
                    )?;
                    // FIXME msgs should go to walletd?
                    endpoints.send_to(
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
                        Request::Success(OptionDetails::with(format!("Connected to {}", source))),
                    ));
                    self.spawning_services.remove(&source);
                }
            }

            Request::SwapOutcome(success) => {
                let swapid = get_swap_id(&source)?;
                self.clean_up_after_swap(&swapid, endpoints)?;
                self.stats.incr_outcome(&success);
                match success {
                    Outcome::Buy => {
                        debug!("Success on swap {}", &swapid);
                    }
                    Outcome::Refund => {
                        warn!("Refund on swap {}", &swapid);
                    }
                    Outcome::Punish => {
                        warn!("Punish on swap {}", &swapid);
                    }
                }
                self.stats.success_rate();
            }

            Request::LaunchSwap(LaunchSwap {
                local_trade_role,
                public_offer,
                local_params,
                swap_id,
                remote_commit,
                funding_address,
            }) => {
                let offerid = public_offer.offer.id();
                let peerd_id = self.peerd_ids.get(&offerid); // Some for Maker after TakerCommit, None for Taker
                let peer: ServiceId = match local_trade_role {
                    TradeRole::Maker if peerd_id.is_some() => peerd_id.unwrap().clone(),
                    TradeRole::Taker => internet2::RemoteNodeAddr {
                        node_id: public_offer.node_id,
                        remote_addr: internet2::RemoteSocketAddr::Ftcp(public_offer.peer_address),
                    }
                    .to_node_addr(internet2::LIGHTNING_P2P_DEFAULT_PORT)
                    .ok_or(internet2::presentation::Error::InvalidEndpoint)?
                    .into(),
                    _ => {
                        error!("peerd_id must exist for Maker after TakerCommit msg!");
                        return Ok(());
                    }
                };
                if self.public_offers.remove(&public_offer) {
                    trace!(
                        "launching swapd with swap_id: {}",
                        swap_id.bright_yellow_bold()
                    );

                    self.consumed_offers
                        .insert(public_offer.offer.id(), swap_id);
                    self.stats.incr_initiated();
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
                    trace!("Received expected peer keys, injecting key in request");
                    let req = if let Request::MakeOffer(mut req) = request {
                        req.peer_secret_key = Some(sk);
                        req.peer_public_key = Some(pk);
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
                    self.handle_rpc_ctl(endpoints, source, req)?
                } else {
                    error!("Received unexpected peer keys");
                }
            }

            Request::GetInfo => {
                endpoints.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Farcasterd, // source
                    source,                // destination
                    Request::NodeInfo(NodeInfo {
                        node_ids: self.node_ids(),
                        listens: self.listens.values().into_iter().cloned().collect(),
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
                endpoints.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Farcasterd, // source
                    source,                // destination
                    Request::PeerList(self.connections.iter().cloned().collect()),
                )?;
            }

            Request::ListSwaps => {
                endpoints.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Farcasterd, // source
                    source,                // destination
                    Request::SwapList(self.running_swaps.iter().cloned().collect()),
                )?;
            }

            // TODO: only list offers matching list of OfferIds
            Request::ListOffers => {
                let pub_offers = self
                    .public_offers
                    .iter()
                    .filter(|k| !self.consumed_offers_contains(&k.offer.id()))
                    .cloned()
                    .collect();
                endpoints.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Farcasterd, // source
                    source,                // destination
                    Request::OfferList(pub_offers),
                )?;
            }

            Request::RevokeOffer(public_offer) => {
                if self.public_offers.remove(&public_offer) {
                    info!("Revoked offer {}", public_offer);
                    endpoints.send_to(
                        ServiceBus::Ctl,
                        ServiceId::Farcasterd,
                        source,
                        Request::String("Successfully revoked offer.".to_string()),
                    )?;
                } else {
                    endpoints.send_to(
                        ServiceBus::Ctl,
                        ServiceId::Farcasterd,
                        source,
                        Request::Failure(Failure {
                            code: 1,
                            info: "Coulod not find to be revoked offer.".to_string(),
                        }),
                    )?;
                }
            }

            // Request::ListOfferIds => {
            //     endpoints.send_to(
            //         ServiceBus::Ctl,
            //         ServiceId::Farcasterd, // source
            //         source,                // destination
            //         Request::OfferIdList(self.public_offers.iter().map(|public_offer| public_offer.id()).collect()),
            //     )?;
            // }
            Request::ListListens => {
                let listen_url: List<String> = List::from_iter(
                    self.listens
                        .clone()
                        .values()
                        .map(|listen| listen.to_url_string()),
                );
                endpoints.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Farcasterd, // source
                    source,                // destination
                    Request::ListenList(listen_url),
                )?;
            }

            Request::MakeOffer(request::ProtoPublicOffer {
                offer,
                public_addr,
                bind_addr,
                peer_secret_key,
                peer_public_key,
                arbitrating_addr,
                accordant_addr,
            }) => {
                let (bindaddr, peer_public_key) = if let Some((pk, bindaddr)) = self
                    .listens
                    .iter()
                    .find(|(_, a)| a == &&bind_addr)
                    .and_then(|(k, v)| self.node_ids.get(k).map(|pk| (pk, v)))
                {
                    (Some(bindaddr), Some(*pk))
                } else {
                    (None, peer_public_key.clone())
                };
                let resp = match (bindaddr, peer_secret_key, peer_public_key) {
                    (None, None, None) => {
                        trace!("Push MakeOffer to pending_requests and requesting a secret from Wallet");
                        return self.get_secret(endpoints, source, request);
                    }
                    (None, Some(sk), Some(pk)) => {
                        self.listens.insert(offer.id(), bind_addr);
                        self.node_ids.insert(offer.id(), pk);
                        info!(
                            "{} for incoming peer connections on {}",
                            "Starting listener".bright_blue_bold(),
                            bind_addr.bright_blue_bold()
                        );
                        self.listen(&bind_addr, sk)
                    }
                    (Some(&addr), _, Some(pk)) => {
                        // no need for the keys, because peerd already knows them
                        self.listens.insert(offer.id(), addr);
                        self.node_ids.insert(offer.id(), pk);
                        let msg = format!("Already listening on {}", &bind_addr);
                        debug!("{}", &msg);
                        Ok(())
                    }
                    _ => unreachable!(),
                };
                let res = match resp {
                    Ok(_) => {
                        info!(
                            "Connection daemon {} for incoming peer connections on {}",
                            "listens".bright_green_bold(),
                            bind_addr
                        );
                        let node_id = self.node_ids.get(&offer.id()).cloned().unwrap();
                        let public_offer = offer.to_public_v1(node_id, public_addr.into());
                        let pub_offer_id = public_offer.id();
                        let serialized_offer = public_offer.to_string();
                        if !self.public_offers.insert(public_offer) {
                            let msg = s!("This Public offer was previously registered");
                            error!("{}", msg.err());
                            return Err(Error::Other(msg));
                        }
                        let msg = format!(
                            "{} {}",
                            "Public offer registered, please share with taker: ".bright_blue_bold(),
                            serialized_offer.bright_yellow_bold()
                        );
                        info!(
                            "{}: {:#}",
                            "Public offer registered".bright_green_bold(),
                            pub_offer_id.bright_yellow_bold()
                        );
                        report_to.push((
                            Some(source.clone()),
                            Request::Success(OptionDetails(Some(msg))),
                        ));
                        self.arb_addrs.insert(pub_offer_id, arbitrating_addr);
                        self.acc_addrs
                            .insert(pub_offer_id, monero::Address::from_str(&accordant_addr)?);
                        Ok(())
                    }
                    Err(err) => {
                        error!("{}", err.err());
                        Err(err)
                    }
                };
                if let Err(err) = res {
                    report_to.push((
                        Some(source.clone()),
                        Request::Failure(Failure {
                            code: 1,
                            info: err.to_string(),
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
                    || self.consumed_offers_contains(&public_offer.offer.id())
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
                        .to_node_addr(internet2::LIGHTNING_P2P_DEFAULT_PORT)
                        .ok_or(internet2::presentation::Error::InvalidEndpoint)?;

                    // Connect
                    let peer_connected_is_ok =
                        match (self.connections.contains(&peer), peer_secret_key) {
                            (false, None) => return self.get_secret(endpoints, source, request),
                            (false, Some(sk)) => {
                                debug!(
                                    "{} to remote peer {}",
                                    "Connecting".bright_blue_bold(),
                                    peer.bright_blue_italic()
                                );
                                match self.connect_peer(source.clone(), &peer, sk) {
                                    Err(err) => {
                                        report_to.push((
                                            Some(source.clone()),
                                            Request::Failure(Failure {
                                                code: 1,
                                                info: err.to_string(),
                                            }),
                                        ));
                                        false
                                    }
                                    Ok(()) => true,
                                }
                            }
                            (true, _) => {
                                let msg = format!(
                                    "Already connected to remote peer {}",
                                    peer.bright_blue_italic()
                                );
                                warn!("{}", &msg);
                                true
                            }
                        };

                    if peer_connected_is_ok {
                        let offer_registered = format!(
                            "{}: {:#}",
                            "Public offer registered".bright_green_bold(),
                            &public_offer.id().bright_yellow_bold()
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
                        endpoints.send_to(
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
                if let Some(queue) = self.progress.get_mut(&ServiceId::Swap(swapid)) {
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
                    }
                } else {
                    let info = if self.making_swaps.contains_key(&ServiceId::Swap(swapid))
                        || self.taking_swaps.contains_key(&ServiceId::Swap(swapid))
                    {
                        s!("No progress made yet on this swap")
                    } else {
                        s!("Unknown swapd")
                    };
                    endpoints.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        source,
                        Request::Failure(Failure { code: 1, info }),
                    )?;
                }
            }
            Request::FundingInfo(info) => match info {
                FundingInfo::Bitcoin(BitcoinFundingInfo {
                    swap_id,
                    address,
                    amount,
                }) => {
                    self.stats.incr_awaiting_funding(&Coin::Bitcoin);
                    let network = match address.network {
                        bitcoin::Network::Bitcoin => Network::Mainnet,
                        bitcoin::Network::Testnet => Network::Testnet,
                        bitcoin::Network::Signet => Network::Testnet,
                        bitcoin::Network::Regtest => Network::Local,
                    };
                    if let Some(auto_fund_config) = self.config.get_auto_funding_config(network) {
                        info!(
                            "{} | Attempting to auto-fund Bitcoin",
                            swap_id.bright_blue_italic()
                        );
                        debug!(
                            "{} | Auto funding config: {:#?}",
                            swap_id.bright_blue_italic(),
                            auto_fund_config
                        );

                        use bitcoincore_rpc::{Auth, Client, Error, RpcApi};
                        use std::env;
                        use std::path::PathBuf;
                        use std::str::FromStr;

                        let host = auto_fund_config.bitcoin_rpc;
                        let bitcoin_rpc = match auto_fund_config.bitcoin_cookie_path {
                            Some(cookie) => {
                                let path = PathBuf::from_str(&shellexpand::tilde(&cookie)).unwrap();
                                debug!("{} | bitcoin-rpc connecting with cookie auth",
                                       swap_id.bright_blue_italic());
                                Client::new(&host, Auth::CookieFile(path))
                            }
                            None => {
                                match (auto_fund_config.bitcoin_rpc_user, auto_fund_config.bitcoin_rpc_pass) {
                                    (Some(rpc_user), Some(rpc_pass)) => {
                                        debug!("{} | bitcoin-rpc connecting with userpass auth",
                                               swap_id.bright_blue_italic());
                                        Client::new(&host, Auth::UserPass(rpc_user, rpc_pass))
                                    }
                                    _ => {
                                        error!(
                                            "{} | Couldn't instantiate Bitcoin RPC - provide either `bitcoin_cookie_path` or `bitcoin_rpc_user` AND `bitcoin_rpc_pass` configuration parameters",
                                            swap_id.bright_blue_italic()
                                        );

                                        Err(Error::InvalidCookieFile)}
                                }
                            }
                        }.unwrap();

                        match bitcoin_rpc
                            .send_to_address(&address, amount, None, None, None, None, None, None)
                        {
                            Ok(txid) => {
                                info!(
                                    "{} | Auto-funded Bitcoin with txid: {}",
                                    swap_id.bright_blue_italic(),
                                    txid
                                );
                                self.funding_btc.insert(swap_id, (address, amount, true));
                            }
                            Err(err) => {
                                warn!("{}", err);
                                error!(
                                    "{} | Auto-funding Bitcoin transaction failed, pushing to cli, use `swap-cli needs-funding Bitcoin` to retrieve address and amount",
                                    swap_id.bright_blue_italic()
                                );
                                self.funding_btc.insert(swap_id, (address, amount, false));
                            }
                        }
                    } else {
                        self.funding_btc.insert(swap_id, (address, amount, false));
                    }
                }
                FundingInfo::Monero(MoneroFundingInfo {
                    swap_id,
                    address,
                    amount,
                }) => {
                    self.stats.incr_awaiting_funding(&Coin::Monero);
                    let network = match address.network {
                        monero::Network::Mainnet => Network::Mainnet,
                        monero::Network::Stagenet => Network::Testnet,
                        monero::Network::Testnet => Network::Local,
                    };
                    if let Some(auto_fund_config) = self.config.get_auto_funding_config(network) {
                        info!(
                            "{} | Attempting to auto-fund Monero",
                            swap_id.bright_blue_italic()
                        );
                        debug!(
                            "{} | Auto funding config: {:#?}",
                            swap_id.bright_blue_italic(),
                            auto_fund_config
                        );
                        use tokio::runtime::Builder;
                        let rt = Builder::new_multi_thread()
                            .worker_threads(1)
                            .enable_all()
                            .build()
                            .unwrap();
                        rt.block_on(async {
                            let host = auto_fund_config.monero_rpc_wallet;
                            let wallet_client =
                                monero_rpc::RpcClient::new(host);
                            let wallet = wallet_client.wallet();
                            let options = monero_rpc::TransferOptions::default();
                            match wallet
                                .transfer(
                                    [(address, amount)].iter().cloned().collect(),
                                    monero_rpc::TransferPriority::Default,
                                    options.clone(),
                                )
                                .await
                            {
                                Ok(tx) => {
                                    info!(
                                        "{} | Auto-funded Monero with txid: {}",
                                        &swap_id.bright_blue_italic(),
                                        tx.tx_hash.to_string()
                                    );
                                    self.funding_xmr.insert(swap_id, (address, amount, true));
                                }
                                Err(err) => {
                                    warn!("{}", err);
                                    error!("{} | Auto-funding Monero transaction failed, pushing to cli, use `swap-cli needs-funding Monero` to retrieve address and amount", &swap_id.bright_blue_italic());
                                    self.funding_xmr.insert(swap_id, (address, amount, false));
                                }
                            }
                        });
                    } else {
                        self.funding_xmr.insert(swap_id, (address, amount, false));
                    }
                }
            },

            Request::FundingCompleted(coin) => {
                let swapid = get_swap_id(&source)?;
                if match coin {
                    Coin::Bitcoin => self.funding_btc.remove(&get_swap_id(&source)?).is_some(),
                    Coin::Monero => self.funding_xmr.remove(&get_swap_id(&source)?).is_some(),
                } {
                    self.stats.incr_funded(&coin);
                    info!(
                        "{} | Your {} funding completed",
                        swapid.bright_blue_italic(),
                        coin.bright_green_bold()
                    );
                }
            }

            Request::FundingCanceled(coin) => {
                let swapid = get_swap_id(&source)?;
                if match coin {
                    Coin::Bitcoin => self.funding_btc.remove(&get_swap_id(&source)?).is_some(),
                    Coin::Monero => self.funding_xmr.remove(&get_swap_id(&source)?).is_some(),
                } {
                    self.stats.incr_funding_monero_canceled();
                    info!(
                        "{} | Your {} funding was canceled",
                        swapid.bright_blue_italic(),
                        coin.bright_green_bold()
                    );
                }
            }

            Request::NeedsFunding(Coin::Monero) => {
                let len = self.funding_xmr.len();
                let res = self
                    .funding_xmr
                    .iter()
                    .filter(|(_, (_, _, autofund))| !*autofund)
                    .enumerate()
                    .map(|(i, (swap_id, (address, amount, _)))| {
                        let mut res = format!(
                            "{}",
                            MoneroFundingInfo {
                                swap_id: *swap_id,
                                amount: *amount,
                                address: *address,
                            }
                        );
                        if i < len - 1 {
                            res.push('\n');
                        }
                        res
                    })
                    .collect();
                endpoints.send_to(
                    ServiceBus::Ctl,
                    self.identity(),
                    source,
                    Request::String(res),
                )?;
            }
            Request::NeedsFunding(Coin::Bitcoin) => {
                let len = self.funding_btc.len();
                let res = self
                    .funding_btc
                    .iter()
                    .filter(|(_, (_, _, autofund))| !*autofund)
                    .enumerate()
                    .map(|(i, (swap_id, (address, amount, _)))| {
                        let mut res = format!(
                            "{}",
                            BitcoinFundingInfo {
                                swap_id: *swap_id,
                                amount: *amount,
                                address: address.clone(),
                            }
                        );
                        if i < len - 1 {
                            res.push('\n');
                        }
                        res
                    })
                    .collect();
                endpoints.send_to(
                    ServiceBus::Ctl,
                    self.identity(),
                    source,
                    Request::String(res),
                )?;
            }

            Request::PeerdTerminated => {
                if let ServiceId::Peer(addr) = source {
                    if self.connections.remove(&addr) {
                        debug!(
                            "removed connection {} from farcasterd registered connections",
                            addr
                        );
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
                endpoints.send_to(ServiceBus::Ctl, self.identity(), respond_to, resp)?;
            }
        }
        debug!("processed all cli notifications");
        Ok(())
    }

    fn listen(&mut self, addr: &RemoteSocketAddr, sk: SecretKey) -> Result<(), Error> {
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
                    &format!("{}", sk.display_secret()),
                    "--token",
                    &self.wallet_token.clone().to_string(),
                ],
            )?;
            debug!("New instance of peerd launched with PID {}", child.id());
            Ok(())
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
    ) -> Result<(), Error> {
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
                &format!("{}", sk.display_secret()),
                "--token",
                &self.wallet_token.clone().to_string(),
            ],
        );

        // in case it can't connect wait for it to crash
        std::thread::sleep(Duration::from_secs_f32(0.5));

        // status is Some if peerd returns because it crashed
        let (child, status) = child.and_then(|mut c| c.try_wait().map(|s| (c, s)))?;

        if status.is_some() {
            return Err(Error::Peer(internet2::presentation::Error::InvalidEndpoint));
        }

        debug!("New instance of peerd launched with PID {}", child.id());

        self.spawning_services
            .insert(ServiceId::Peer(node_addr.clone()), source);
        debug!("Awaiting for peerd to connect...");

        Ok(())
    }

    fn get_secret(
        &mut self,
        endpoints: &mut Endpoints,
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
        endpoints.send_to(
            ServiceBus::Ctl,
            ServiceId::Farcasterd,
            ServiceId::Wallet,
            Request::GetKeys(wallet_token),
        )?;
        Ok(())
    }
}

fn syncers_up(
    source: ServiceId,
    spawning_services: &mut HashMap<ServiceId, ServiceId>,
    services: &HashMap<(Coin, Network), ServiceId>,
    clients: &mut HashMap<(Coin, Network), HashSet<SwapId>>,
    coin: Coin,
    network: Network,
    swap_id: SwapId,
    config: &Config,
) -> Result<(), Error> {
    let k = (coin, network);
    let s = ServiceId::Syncer(coin, network);
    if !services.contains_key(&k) && !spawning_services.contains_key(&s) {
        let mut args = vec![
            "--coin".to_string(),
            coin.to_string(),
            "--network".to_string(),
            network.to_string(),
        ];
        args.append(&mut syncer_servers_args(config, coin, network)?);
        info!("launching syncer with: {:?}", args);
        launch("syncerd", args)?;
        clients.insert(k, none!());
        spawning_services.insert(s, source);
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
    debug!("{}", msg);

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

/// Return the list of needed arguments for a syncer given a config and a network.
/// This function only register the minimal set of URLs needed for the blockchain to work.
fn syncer_servers_args(config: &Config, coin: Coin, net: Network) -> Result<Vec<String>, Error> {
    match config.get_syncer_servers(net) {
        Some(servers) => match coin {
            Coin::Bitcoin => Ok(vec![
                "--electrum-server".to_string(),
                servers.electrum_server,
            ]),
            Coin::Monero => {
                let mut args: Vec<String> = vec![
                    "--monero-daemon".to_string(),
                    servers.monero_daemon,
                    "--monero-rpc-wallet".to_string(),
                    servers.monero_rpc_wallet,
                ];
                args.extend(
                    servers
                        .monero_lws
                        .map_or(vec![], |v| vec!["--monero-lws".to_string(), v]),
                );
                args.extend(
                    servers
                        .monero_wallet_dir
                        .map_or(vec![], |v| vec!["--monero-wallet-dir-path".to_string(), v]),
                );
                Ok(args)
            }
        },
        None => Err(SyncerError::InvalidConfig.into()),
    }
}

pub fn launch(
    name: &str,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> io::Result<process::Child> {
    let app = Opts::command();
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

    // Forwarded shared options from farcasterd to launched microservices
    // Cannot use value_of directly because of default values
    let matches = app.get_matches();

    // Set verbosity to same level
    let verbose = matches.occurrences_of("verbose");
    if verbose > 0 {
        cmd.args(&[&format!(
            "-{}",
            (0..verbose).map(|_| "v").collect::<String>()
        )]);
    }

    if let Some(d) = &matches.value_of("data-dir") {
        cmd.args(&["-d", d]);
    }

    if let Some(m) = &matches.value_of("msg-socket") {
        cmd.args(&["-m", m]);
    }

    if let Some(x) = &matches.value_of("ctl-socket") {
        cmd.args(&["-x", x]);
    }

    // Forward tor proxy argument
    let parsed = Opts::parse();
    match &parsed.shared.tor_proxy {
        Some(None) => {
            cmd.args(&["-T"]);
        }
        Some(Some(val)) => {
            cmd.args(&["-T", &format!("{}", val)]);
        }
        _ => (),
    }

    // Given specialized args in launch
    cmd.args(args);

    debug!("Executing `{:?}`", cmd);
    cmd.spawn().map_err(|err| {
        error!("Error launching {}: {}", name, err);
        err
    })
}
