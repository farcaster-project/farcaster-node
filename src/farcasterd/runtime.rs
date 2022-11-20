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

use crate::bus::ctl::{BitcoinFundingInfo, CtlMsg, GetKeys, MoneroFundingInfo};
use crate::bus::p2p::{PeerMsg, TakerCommit};
use crate::bus::sync::SyncMsg;
use crate::bus::{BusMsg, List, ServiceBus};
use crate::event::StateMachineExecutor;
use crate::farcasterd::stats::Stats;
use crate::farcasterd::syncer_state_machine::{SyncerStateMachine, SyncerStateMachineExecutor};
use crate::farcasterd::trade_state_machine::{TradeStateMachine, TradeStateMachineExecutor};
use crate::farcasterd::Opts;
use crate::syncerd::{Event as SyncerEvent, SweepSuccess, TaskId};
use crate::{
    bus::ctl::{Keys, LaunchSwap, ProgressStack, Token},
    bus::info::{InfoMsg, NodeInfo, OfferInfo, OfferStatusSelector, ProgressEvent, SwapProgress},
    bus::{Failure, FailureCode, Progress},
    clap::Parser,
    error::SyncerError,
    service::Endpoints,
};
use crate::{Config, CtlServer, Error, LogStyle, Service, ServiceConfig, ServiceId};

use std::collections::VecDeque;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io;
use std::iter::FromIterator;
use std::process;
use std::time::{Duration, SystemTime};

use bitcoin::{hashes::hex::ToHex, secp256k1::PublicKey, secp256k1::SecretKey};
use clap::IntoApp;
use farcaster_core::{
    blockchain::{Blockchain, Network},
    role::TradeRole,
    swap::btcxmr::PublicOffer,
    swap::SwapId,
};
use internet2::addr::NodeId;
use internet2::{addr::InetSocketAddr, addr::NodeAddr};
use microservices::esb::{self, Handler};

pub fn run(
    service_config: ServiceConfig,
    config: Config,
    _opts: Opts,
    wallet_token: Token,
) -> Result<(), Error> {
    let _walletd = launch("walletd", &["--token", &wallet_token.to_string()])?;
    if config.is_grpc_enable() {
        let _grpcd = launch(
            "grpcd",
            &[
                "--grpc-port",
                &config
                    .farcasterd
                    .clone()
                    .unwrap()
                    .grpc
                    .unwrap()
                    .port
                    .to_string(),
            ],
        )?;
    }
    let empty: Vec<String> = vec![];
    let _databased = launch("databased", empty)?;

    if config.is_auto_funding_enable() {
        info!(
            "{} will attempt to {}",
            "farcasterd".label(),
            "fund automatically".label()
        );
    }

    let runtime = Runtime {
        identity: ServiceId::Farcasterd,
        node_secret_key: None,
        node_public_key: None,
        listens: none!(),
        started: SystemTime::now(),
        spawning_services: none!(),
        registered_services: none!(),
        public_offers: none!(),
        wallet_token,
        progress: none!(),
        progress_subscriptions: none!(),
        stats: none!(),
        config,
        syncer_task_counter: 0,
        trade_state_machines: vec![],
        syncer_state_machines: none!(),
    };

    let broker = true;
    Service::run(service_config, runtime, broker)
}

pub struct Runtime {
    identity: ServiceId,                         // Set on Runtime instantiation
    wallet_token: Token,                         // Set on Runtime instantiation
    started: SystemTime,                         // Set on Runtime instantiation
    node_secret_key: Option<SecretKey>, // Set by Keys request shortly after Hello from walletd
    node_public_key: Option<PublicKey>, // Set by Keys request shortly after Hello from walletd
    pub listens: HashSet<InetSocketAddr>, // Set by MakeOffer, contains unique socket addresses of the binding peerd listeners.
    pub spawning_services: HashSet<ServiceId>, // Services that have been launched, but have not replied with Hello yet
    pub registered_services: HashSet<ServiceId>, // Services that have announced themselves with Hello
    pub public_offers: HashSet<PublicOffer>, // The set of all known public offers. Includes open, consumed and ended offers includes open, consumed and ended offers
    progress: HashMap<ServiceId, VecDeque<ProgressStack>>, // A mapping from Swap ServiceId to its sent and received progress messages (Progress, Success, Failure)
    progress_subscriptions: HashMap<ServiceId, HashSet<ServiceId>>, // A mapping from a Client ServiceId to its subsribed swap progresses
    pub stats: Stats,             // Some stats about offers and swaps
    pub config: Config,           // Configuration for syncers, auto-funding, and grpc
    pub syncer_task_counter: u32, // A strictly incrementing counter of issued syncer tasks
    pub trade_state_machines: Vec<TradeStateMachine>, // New trade state machines are inserted on creation and destroyed upon state machine end transitions
    syncer_state_machines: HashMap<TaskId, SyncerStateMachine>, // New syncer state machines are inserted by their syncer task id when sending a syncer request and destroyed upon matching syncer request receival
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
            // Peer-to-peer message bus, only accept Peer message
            (ServiceBus::Msg, BusMsg::P2p(req)) => self.handle_msg(endpoints, source, req),
            // Control bus for issuing control commands, only accept Ctl message
            (ServiceBus::Ctl, BusMsg::Ctl(req)) => self.handle_ctl(endpoints, source, req),
            // Info command bus, only accept Info message
            (ServiceBus::Info, BusMsg::Info(req)) => self.handle_info(endpoints, source, req),
            // Syncer event bus for blockchain tasks and events, only accept Sync message
            (ServiceBus::Sync, BusMsg::Sync(req)) => self.handle_sync(endpoints, source, req),
            // All other pairs are not supported
            (_, request) => Err(Error::NotSupported(bus, request.to_string())),
        }
    }

    fn handle_err(
        &mut self,
        endpoints: &mut Endpoints,
        err: esb::Error<ServiceId>,
    ) -> Result<(), Error> {
        // If the client routes through farcasterd, but the target daemon does not exist, send a response back to the client
        match err {
            esb::Error::Send(ServiceId::Client(client_id), target, ..) => {
                debug!(
                    "Target service {} not found while routing msg from {}",
                    target,
                    ServiceId::Client(client_id)
                );
                self.send_client_ctl(
                    endpoints,
                    ServiceId::Client(client_id),
                    CtlMsg::Failure(Failure {
                        code: FailureCode::TargetServiceNotFound,
                        info: format!("The target service {} does not exist", target),
                    }),
                )?;
            }
            esb::Error::Send(ServiceId::GrpcdClient(client_id), target, ..) => {
                debug!(
                    "Target service {} not found while routing msg from grpc server",
                    target
                );
                self.send_client_ctl(
                    endpoints,
                    ServiceId::GrpcdClient(client_id),
                    CtlMsg::Failure(Failure {
                        code: FailureCode::TargetServiceNotFound,
                        info: format!("The target service {} does not exist", target),
                    }),
                )?;
            }
            _ => {}
        }

        // We do nothing and do not propagate error; it's already being reported
        // with `error!` macro by the controller. If we propagate error here
        // this will make whole daemon panic
        Ok(())
    }
}

impl Runtime {
    fn handle_msg(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: PeerMsg,
    ) -> Result<(), Error> {
        info!(
            "{} received {} from peer - processing with sms",
            self.identity, request
        );
        self.process_request_with_state_machines(BusMsg::P2p(request), source, endpoints)
    }

    fn handle_ctl(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: CtlMsg,
    ) -> Result<(), Error> {
        match request {
            CtlMsg::Hello => {
                // Ignoring; this is used to set remote identity at ZMQ level
                info!(
                    "Service {} is now {}",
                    source.label(),
                    "connected".bright_green_bold()
                );

                match &source {
                    ServiceId::Farcasterd => {
                        error!(
                            "{}",
                            "Unexpected another farcasterd instance connection".err()
                        );
                    }
                    ServiceId::Database => {
                        self.registered_services.insert(source.clone());
                    }
                    ServiceId::Wallet => {
                        self.registered_services.insert(source.clone());
                        let wallet_token = GetKeys(self.wallet_token.clone());
                        endpoints.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            source.clone(),
                            BusMsg::Ctl(CtlMsg::GetKeys(wallet_token)),
                        )?;
                    }
                    ServiceId::Peer(_) => {
                        // If this is a connecting peerd, only process the
                        // connection once ConnectSuccess / ConnectFailure is
                        // received
                        let awaiting_swaps: Vec<_> = self
                            .trade_state_machines
                            .iter()
                            .filter(|tsm| tsm.awaiting_connect_from() == Some(source.clone()))
                            .map(|tsm| tsm.swap_id().map_or("…".to_string(), |s| s.to_string()))
                            .collect();
                        if !awaiting_swaps.is_empty() {
                            info!("Received hello from awaited peerd connection {}, will continue processing once swaps {:?} are connected.", source, awaiting_swaps);
                        } else {
                            self.handle_new_connection(source.clone());
                        }
                    }
                    ServiceId::Swap(_) => {
                        // nothing to do, we register swapd instances on a by-swap basis
                    }
                    ServiceId::Syncer(_, _) => {
                        if self.spawning_services.remove(&source) {
                            info!(
                                "Syncer {} is registered; total {} syncers are known",
                                source,
                                self.count_syncers().bright_blue_bold()
                            );
                            self.registered_services.insert(source.clone());
                        } else {
                            error!(
                                "Syncer {} was already registered; the service probably was relaunched\\
                                 externally, or maybe multiple syncers launched?",
                                source
                            );
                        }
                    }
                    _ => {
                        // Ignoring the rest of daemon/client types
                    }
                };

                // For the HELLO messages we have to check if any of the state machines have to be updated
                // We need to move them first in order to not retain ownership over self.
                let mut moved_trade_state_machines = self
                    .trade_state_machines
                    .drain(..)
                    .collect::<Vec<TradeStateMachine>>();
                for tsm in moved_trade_state_machines.drain(..) {
                    if let Some(new_tsm) = TradeStateMachineExecutor::execute(
                        self,
                        endpoints,
                        source.clone(),
                        BusMsg::Ctl(request.clone()),
                        tsm,
                    )? {
                        self.trade_state_machines.push(new_tsm);
                    }
                }
                let mut moved_syncer_state_machines = self
                    .syncer_state_machines
                    .drain()
                    .collect::<Vec<(TaskId, SyncerStateMachine)>>();
                for (task_id, ssm) in moved_syncer_state_machines.drain(..) {
                    if let Some(new_ssm) = SyncerStateMachineExecutor::execute(
                        self,
                        endpoints,
                        source.clone(),
                        BusMsg::Ctl(request.clone()),
                        ssm,
                    )? {
                        self.syncer_state_machines.insert(task_id, new_ssm);
                    }
                }
            }

            CtlMsg::Keys(Keys(sk, pk)) => {
                debug!("received peerd keys {}", sk.display_secret());
                self.node_secret_key = Some(sk);
                self.node_public_key = Some(pk);
            }

            CtlMsg::PeerdTerminated if matches!(source, ServiceId::Peer(_)) => {
                self.handle_failed_connection(endpoints, source.clone())?;

                // log a message if a swap running over this connection
                // is not completed, and thus present in consumed_offers
                if self.connection_has_swap_client(&source) {
                    info!("A swap is still running over the terminated peer {}, the counterparty will attempt to reconnect.", source.bright_blue_italic());
                }
            }

            // Notify all swapds in case of disconnect
            CtlMsg::Disconnected => {
                for swap_id in self.trade_state_machines.iter().filter_map(|tsm| {
                    if let Some(peer) = tsm.get_connection() {
                        if peer == source {
                            tsm.swap_id()
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }) {
                    endpoints.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        ServiceId::Swap(swap_id.clone()),
                        BusMsg::Ctl(CtlMsg::Disconnected),
                    )?;
                }
            }

            // Notify all swapds in case of reconnect
            CtlMsg::Reconnected => {
                for swap_id in self.trade_state_machines.iter().filter_map(|tsm| {
                    if let Some(peer) = tsm.get_connection() {
                        if peer == source {
                            tsm.swap_id()
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }) {
                    endpoints.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        ServiceId::Swap(swap_id.clone()),
                        BusMsg::Ctl(CtlMsg::Reconnected),
                    )?;
                }
            }

            // Add progress in queues and forward to subscribed clients
            event @ (CtlMsg::Progress(..) | CtlMsg::Success(..) | CtlMsg::Failure(..)) => {
                if !self.progress.contains_key(&source) {
                    self.progress.insert(source.clone(), none!());
                };
                let queue = self.progress.get_mut(&source).expect("checked/added above");
                let prog = match event {
                    CtlMsg::Progress(p) => {
                        (ProgressStack::Progress(p.clone()), InfoMsg::Progress(p))
                    }
                    CtlMsg::Success(s) => (ProgressStack::Success(s.clone()), InfoMsg::Success(s)),
                    CtlMsg::Failure(f) => (ProgressStack::Failure(f.clone()), InfoMsg::Failure(f)),
                    // filtered at higher level
                    _ => unreachable!(),
                };
                queue.push_back(prog.0);
                // forward the request to each subscribed clients
                self.notify_subscribed_clients(endpoints, &source, prog.1);
            }

            req => {
                self.process_request_with_state_machines(BusMsg::Ctl(req), source, endpoints)?;
            }
        }

        Ok(())
    }

    fn handle_info(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: InfoMsg,
    ) -> Result<(), Error> {
        let mut report_to: Vec<(Option<ServiceId>, InfoMsg)> = none!();

        match request {
            InfoMsg::GetInfo => {
                self.send_client_info(
                    endpoints,
                    source,
                    InfoMsg::NodeInfo(NodeInfo {
                        listens: self.listens.iter().into_iter().cloned().collect(),
                        uptime: SystemTime::now()
                            .duration_since(self.started)
                            .unwrap_or_else(|_| Duration::from_secs(0)),
                        since: self
                            .started
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap_or_else(|_| Duration::from_secs(0))
                            .as_secs(),
                        peers: self.get_open_connections(),
                        swaps: self
                            .trade_state_machines
                            .iter()
                            .filter_map(|tsm| tsm.swap_id())
                            .collect(),
                        offers: self
                            .trade_state_machines
                            .iter()
                            .filter_map(|tsm| tsm.open_offer())
                            .collect(),
                        stats: self.stats.clone(),
                    }),
                )?;
            }

            InfoMsg::ListPeers => {
                self.send_client_info(
                    endpoints,
                    source,
                    InfoMsg::PeerList(self.get_open_connections().into()),
                )?;
            }

            InfoMsg::ListSwaps => {
                self.send_client_info(
                    endpoints,
                    source,
                    InfoMsg::SwapList(
                        self.trade_state_machines
                            .iter()
                            .filter_map(|tsm| tsm.swap_id())
                            .collect(),
                    ),
                )?;
            }

            InfoMsg::ListOffers(ref offer_status_selector) => {
                match offer_status_selector {
                    OfferStatusSelector::Open => {
                        let open_offers = self
                            .trade_state_machines
                            .iter()
                            .filter_map(|tsm| tsm.open_offer())
                            .map(|offer| OfferInfo {
                                offer: offer.to_string(),
                                details: offer.clone(),
                            })
                            .collect();
                        self.send_client_info(endpoints, source, InfoMsg::OfferList(open_offers))?;
                    }
                    OfferStatusSelector::InProgress => {
                        let pub_offers = self
                            .public_offers
                            .iter()
                            .filter(|k| self.consumed_offers_contains(k))
                            .map(|offer| OfferInfo {
                                offer: offer.to_string(),
                                details: offer.clone(),
                            })
                            .collect();
                        self.send_client_info(endpoints, source, InfoMsg::OfferList(pub_offers))?;
                    }
                    _ => {
                        // Forward the request to database service
                        endpoints.send_to(
                            ServiceBus::Info,
                            source,
                            ServiceId::Database,
                            BusMsg::Info(request),
                        )?;
                    }
                };
            }

            InfoMsg::ListListens => {
                let listen_url: List<String> =
                    List::from_iter(self.listens.clone().iter().map(|listen| listen.to_string()));
                self.send_client_info(endpoints, source, InfoMsg::ListenList(listen_url))?;
            }

            // Returns a unique response that contains the complete progress queue
            InfoMsg::ReadProgress(swap_id) => {
                if let Some(queue) = self.progress.get_mut(&ServiceId::Swap(swap_id)) {
                    let mut swap_progress = SwapProgress { progress: vec![] };
                    for req in queue.iter() {
                        match req {
                            ProgressStack::Progress(Progress::Message(m)) => {
                                swap_progress
                                    .progress
                                    .push(ProgressEvent::Message(m.clone()));
                            }
                            ProgressStack::Progress(Progress::StateTransition(t)) => {
                                swap_progress
                                    .progress
                                    .push(ProgressEvent::StateTransition(t.clone()));
                            }
                            ProgressStack::Success(s) => {
                                swap_progress
                                    .progress
                                    .push(ProgressEvent::Success(s.clone()));
                            }
                            ProgressStack::Failure(f) => {
                                swap_progress
                                    .progress
                                    .push(ProgressEvent::Failure(f.clone()));
                            }
                        };
                    }
                    report_to.push((Some(source.clone()), InfoMsg::SwapProgress(swap_progress)));
                } else {
                    let info = if self.running_swaps_contain(&swap_id) {
                        s!("No progress made yet on this swap")
                    } else {
                        s!("Unknown swapd")
                    };
                    report_to.push((
                        Some(source.clone()),
                        InfoMsg::Failure(Failure {
                            code: FailureCode::Unknown,
                            info,
                        }),
                    ));
                }
            }

            // Add the request's source to the subscription list for later progress notifications
            // and send all notifications already in the queue
            InfoMsg::SubscribeProgress(swap_id) => {
                let service = ServiceId::Swap(swap_id);
                // if the swap is known either in the tsm's or progress, attach the client
                // otherwise terminate
                if self.running_swaps_contain(&swap_id) || self.progress.contains_key(&service) {
                    if let Some(subscribed) = self.progress_subscriptions.get_mut(&service) {
                        // ret true if not in the set, false otherwise. Double subscribe is not a
                        // problem as we manage the list in a set.
                        let _ = subscribed.insert(source.clone());
                    } else {
                        let mut subscribed = HashSet::new();
                        subscribed.insert(source.clone());
                        // None is returned, the key was not set as checked before
                        let _ = self
                            .progress_subscriptions
                            .insert(service.clone(), subscribed);
                    }
                    trace!(
                        "{} has been added to {} progress subscription",
                        source.clone(),
                        swap_id
                    );
                    // send all queued notification to the source to catch up
                    if let Some(queue) = self.progress.get_mut(&service) {
                        for req in queue.iter() {
                            report_to.push((
                                Some(source.clone()),
                                match req.clone() {
                                    ProgressStack::Progress(p) => InfoMsg::Progress(p),
                                    ProgressStack::Success(s) => InfoMsg::Success(s),
                                    ProgressStack::Failure(f) => InfoMsg::Failure(f),
                                },
                            ));
                        }
                    }
                } else {
                    // no swap service exists, terminate
                    report_to.push((
                        Some(source.clone()),
                        InfoMsg::Failure(Failure {
                            code: FailureCode::Unknown,
                            info: "Unknown swapd".to_string(),
                        }),
                    ));
                }
            }

            // Remove the request's source from the subscription list of notifications
            InfoMsg::UnsubscribeProgress(swap_id) => {
                let service = ServiceId::Swap(swap_id);
                if let Some(subscribed) = self.progress_subscriptions.get_mut(&service) {
                    // we don't care if the source was not in the set
                    let _ = subscribed.remove(&source);
                    trace!(
                        "{} has been removed from {} progress subscription",
                        source.clone(),
                        swap_id
                    );
                    if subscribed.is_empty() {
                        // we drop the empty set located at the swap index
                        let _ = self.progress_subscriptions.remove(&service);
                    }
                }
                // if no swap service exists no subscription need to be removed
            }

            InfoMsg::NeedsFunding(Blockchain::Monero) => {
                let funding_infos: Vec<MoneroFundingInfo> = self
                    .trade_state_machines
                    .iter()
                    .filter_map(|tsm| tsm.needs_funding_monero())
                    .collect();
                let len = funding_infos.len();
                let res = funding_infos
                    .iter()
                    .enumerate()
                    .map(|(i, funding_info)| {
                        let mut res = format!("{}", funding_info);
                        if i < len - 1 {
                            res.push('\n');
                        }
                        res
                    })
                    .collect();
                self.send_client_info(endpoints, source, InfoMsg::String(res))?;
            }

            InfoMsg::NeedsFunding(Blockchain::Bitcoin) => {
                let funding_infos: Vec<BitcoinFundingInfo> = self
                    .trade_state_machines
                    .iter()
                    .filter_map(|tsm| tsm.needs_funding_bitcoin())
                    .collect();
                let len = funding_infos.len();
                let res = funding_infos
                    .iter()
                    .enumerate()
                    .map(|(i, funding_info)| {
                        let mut res = format!("{}", funding_info);
                        if i < len - 1 {
                            res.push('\n');
                        }
                        res
                    })
                    .collect();
                self.send_client_info(endpoints, source, InfoMsg::String(res))?;
            }

            req => {
                warn!("Ignoring request: {}", req.err());
            }
        }

        for (i, (respond_to, resp)) in report_to.clone().into_iter().enumerate() {
            if let Some(respond_to) = respond_to {
                // do not respond to self
                if respond_to == self.identity() {
                    continue;
                }
                trace!("(#{}) Respond to {}: {}", i, respond_to, resp,);
                self.send_client_info(endpoints, respond_to, resp)?;
            }
        }
        trace!("Processed all cli notifications");

        Ok(())
    }

    fn handle_sync(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: SyncMsg,
    ) -> Result<(), Error> {
        self.process_request_with_state_machines(BusMsg::Sync(request), source, endpoints)
    }

    pub fn services_ready(&self) -> Result<(), Error> {
        if !self.registered_services.contains(&ServiceId::Wallet) {
            Err(Error::Farcaster(
                "Farcaster not ready yet, walletd still starting".to_string(),
            ))
        } else if !self.registered_services.contains(&ServiceId::Database) {
            Err(Error::Farcaster(
                "Farcaster not ready yet, databased still starting".to_string(),
            ))
        } else {
            Ok(())
        }
    }

    pub fn peer_keys_ready(&self) -> Result<(SecretKey, PublicKey), Error> {
        if let (Some(sk), Some(pk)) = (self.node_secret_key, self.node_public_key) {
            Ok((sk, pk))
        } else {
            Err(Error::Farcaster("Peer keys not ready yet".to_string()))
        }
    }

    pub fn handle_new_connection(&mut self, connection: ServiceId) {
        self.spawning_services.remove(&connection);
        if self.registered_services.insert(connection.clone()) {
            info!(
                "Connection {} is registered; total {} connections are known",
                connection.bright_blue_italic(),
                self.count_connections().bright_blue_bold(),
            );
        } else {
            warn!(
                "Connection {} was already registered; the service probably was relaunched",
                connection.bright_blue_italic()
            );
        }
    }

    pub fn handle_failed_connection(
        &mut self,
        endpoints: &mut Endpoints,
        connection: ServiceId,
    ) -> Result<(), Error> {
        info!(
            "Connection {} failed. Removing it from our connection pool and terminating.",
            connection
        );
        self.spawning_services.remove(&connection);
        self.registered_services.remove(&connection);
        endpoints.send_to(
            ServiceBus::Ctl,
            self.identity(),
            connection,
            BusMsg::Ctl(CtlMsg::Terminate),
        )?;
        Ok(())
    }

    pub fn clean_up_after_swap(
        &mut self,
        swap_id: &SwapId,
        endpoints: &mut Endpoints,
    ) -> Result<(), Error> {
        endpoints.send_to(
            ServiceBus::Ctl,
            self.identity(),
            ServiceId::Swap(*swap_id),
            BusMsg::Ctl(CtlMsg::Terminate),
        )?;
        endpoints.send_to(
            ServiceBus::Ctl,
            self.identity(),
            ServiceId::Database,
            BusMsg::Ctl(CtlMsg::RemoveCheckpoint(*swap_id)),
        )?;

        self.registered_services = self
            .registered_services
            .clone()
            .drain()
            .filter(|service| {
                if let ServiceId::Peer(..) = service {
                    if !self.connection_has_swap_client(service) {
                        info!("Terminating {}", service);
                        endpoints
                            .send_to(
                                ServiceBus::Ctl,
                                self.identity(),
                                service.clone(),
                                BusMsg::Ctl(CtlMsg::Terminate),
                            )
                            .is_err()
                    } else {
                        true
                    }
                } else if let ServiceId::Syncer(..) = service {
                    if !self.syncer_has_client(service) {
                        info!("Terminating {}", service);
                        endpoints
                            .send_to(
                                ServiceBus::Ctl,
                                self.identity(),
                                service.clone(),
                                BusMsg::Ctl(CtlMsg::Terminate),
                            )
                            .is_err()
                    } else {
                        true
                    }
                } else {
                    true
                }
            })
            .collect();
        Ok(())
    }

    pub fn consumed_offers_contains(&self, offer: &PublicOffer) -> bool {
        self.trade_state_machines
            .iter()
            .filter_map(|tsm| tsm.consumed_offer())
            .any(|tsm_offer| tsm_offer.offer.id() == offer.offer.id())
    }

    fn running_swaps_contain(&self, swap_id: &SwapId) -> bool {
        self.trade_state_machines
            .iter()
            .filter_map(|tsm| tsm.swap_id())
            .any(|tsm_swap_id| tsm_swap_id == *swap_id)
    }

    pub fn syncer_has_client(&self, syncerd: &ServiceId) -> bool {
        self.trade_state_machines.iter().any(|tsm| {
            tsm.syncers()
                .iter()
                .any(|client_syncer| client_syncer == syncerd)
        }) || self
            .syncer_state_machines
            .values()
            .filter_map(|ssm| ssm.syncer())
            .any(|client_syncer| client_syncer == *syncerd)
    }

    fn count_syncers(&self) -> usize {
        self.registered_services
            .iter()
            .filter(|s| matches!(s, ServiceId::Syncer(..)))
            .count()
    }

    fn connection_has_swap_client(&self, peerd: &ServiceId) -> bool {
        self.trade_state_machines
            .iter()
            .filter_map(|tsm| tsm.get_connection())
            .any(|client_connection| client_connection == *peerd)
    }

    pub fn count_connections(&self) -> usize {
        self.registered_services
            .iter()
            .filter(|s| matches!(s, ServiceId::Peer(..)))
            .count()
    }

    fn get_open_connections(&self) -> Vec<NodeAddr> {
        self.registered_services
            .iter()
            .filter_map(|s| {
                if let ServiceId::Peer(n) = s {
                    Some(*n)
                } else {
                    None
                }
            })
            .collect()
    }

    fn match_request_to_syncer_state_machine(
        &mut self,
        req: BusMsg,
        source: ServiceId,
    ) -> Result<Option<SyncerStateMachine>, Error> {
        match (req, source) {
            (BusMsg::Ctl(CtlMsg::SweepAddress(..)), _) => Ok(Some(SyncerStateMachine::Start)),
            (
                BusMsg::Sync(SyncMsg::Event(SyncerEvent::SweepSuccess(SweepSuccess {
                    id, ..
                }))),
                _,
            ) => Ok(self.syncer_state_machines.remove(&id)),
            _ => Ok(None),
        }
    }

    fn match_request_to_trade_state_machine(
        &mut self,
        req: BusMsg,
        source: ServiceId,
    ) -> Result<Option<TradeStateMachine>, Error> {
        match (req, source) {
            (BusMsg::Ctl(CtlMsg::RestoreCheckpoint(..)), _) => {
                Ok(Some(TradeStateMachine::StartRestore))
            }
            (BusMsg::Ctl(CtlMsg::MakeOffer(..)), _) => Ok(Some(TradeStateMachine::StartMaker)),
            (BusMsg::Ctl(CtlMsg::TakeOffer(..)), _) => Ok(Some(TradeStateMachine::StartTaker)),
            (BusMsg::P2p(PeerMsg::TakerCommit(TakerCommit { public_offer, .. })), _)
            | (BusMsg::Ctl(CtlMsg::RevokeOffer(public_offer)), _) => Ok(self
                .trade_state_machines
                .iter()
                .position(|tsm| {
                    if let Some(tsm_public_offer) = tsm.open_offer() {
                        tsm_public_offer == public_offer
                    } else {
                        false
                    }
                })
                .map(|pos| self.trade_state_machines.remove(pos))),
            (BusMsg::Ctl(CtlMsg::LaunchSwap(LaunchSwap { public_offer, .. })), _) => Ok(self
                .trade_state_machines
                .iter()
                .position(|tsm| {
                    if let Some(tsm_public_offer) = tsm.consumed_offer() {
                        tsm_public_offer == public_offer
                    } else {
                        false
                    }
                })
                .map(|pos| self.trade_state_machines.remove(pos))),
            (BusMsg::Ctl(CtlMsg::ConnectSuccess), ServiceId::Peer(addr))
            | (BusMsg::Ctl(CtlMsg::ConnectFailed), ServiceId::Peer(addr)) => Ok(self
                .trade_state_machines
                .iter()
                .position(|tsm| {
                    if let Some(ServiceId::Peer(tsm_addr)) = tsm.awaiting_connect_from() {
                        addr == tsm_addr
                    } else {
                        false
                    }
                })
                .map(|pos| self.trade_state_machines.remove(pos))),
            (BusMsg::Ctl(CtlMsg::PeerdUnreachable(..)), ServiceId::Swap(swap_id))
            | (BusMsg::Ctl(CtlMsg::FundingInfo(..)), ServiceId::Swap(swap_id))
            | (BusMsg::Ctl(CtlMsg::FundingCanceled(..)), ServiceId::Swap(swap_id))
            | (BusMsg::Ctl(CtlMsg::FundingCompleted(..)), ServiceId::Swap(swap_id))
            | (BusMsg::Ctl(CtlMsg::Connect(swap_id)), _)
            | (BusMsg::Ctl(CtlMsg::SwapOutcome(..)), ServiceId::Swap(swap_id)) => Ok(self
                .trade_state_machines
                .iter()
                .position(|tsm| {
                    if let Some(tsm_swap_id) = tsm.swap_id() {
                        tsm_swap_id == swap_id
                    } else {
                        false
                    }
                })
                .map(|pos| self.trade_state_machines.remove(pos))),
            _ => Ok(None),
        }
    }

    fn process_request_with_state_machines(
        &mut self,
        request: BusMsg,
        source: ServiceId,
        endpoints: &mut Endpoints,
    ) -> Result<(), Error> {
        if let Some(tsm) =
            self.match_request_to_trade_state_machine(request.clone(), source.clone())?
        {
            if let Some(new_tsm) =
                TradeStateMachineExecutor::execute(self, endpoints, source, request, tsm)?
            {
                self.trade_state_machines.push(new_tsm);
            }
            Ok(())
        } else if let Some(ssm) =
            self.match_request_to_syncer_state_machine(request.clone(), source.clone())?
        {
            if let Some(new_ssm) =
                SyncerStateMachineExecutor::execute(self, endpoints, source, request, ssm)?
            {
                if let Some(task_id) = new_ssm.task_id() {
                    self.syncer_state_machines.insert(task_id, new_ssm);
                } else {
                    error!("Cannot process new syncer state machine without a task id");
                }
            }
            Ok(())
        } else {
            match request {
                BusMsg::Ctl(CtlMsg::RevokeOffer(..)) => {
                    endpoints.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        source,
                        BusMsg::Ctl(CtlMsg::Failure(Failure {
                            code: FailureCode::Unknown,
                            info: "Offer to revoke not found.".to_string(),
                        })),
                    )?;
                    Ok(())
                }
                BusMsg::Ctl(CtlMsg::Connect(..)) => {
                    endpoints.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        source,
                        BusMsg::Ctl(CtlMsg::Failure(Failure {
                            code: FailureCode::Unknown,
                            info: "Swap to connect not found.".to_string(),
                        })),
                    )?;
                    Ok(())
                }
                _ => {
                    warn!("Received request {}, but did not process it", request);
                    Ok(())
                }
            }
        }
    }

    pub fn listen(&mut self, bind_addr: InetSocketAddr) -> Result<NodeId, Error> {
        self.services_ready()?;
        let (peer_secret_key, peer_public_key) = self.peer_keys_ready()?;
        let node_id = NodeId::from(peer_public_key);
        if self.listens.iter().any(|a| a == &bind_addr) {
            let msg = format!("Already listening on {}", &bind_addr);
            debug!("{}", &msg);
            return Ok(node_id);
        }
        info!(
            "{} for incoming peer connections on {}",
            "Starting listener".bright_blue_bold(),
            bind_addr.bright_blue_bold()
        );

        let address = bind_addr.address();
        let port = bind_addr.port().ok_or(Error::Farcaster(
            "listen requires the port to listen on".to_string(),
        ))?;

        debug!("Instantiating peerd...");
        let child = launch(
            "peerd",
            &[
                "--listen",
                &format!("{}", address),
                "--port",
                &port.to_string(),
                "--peer-secret-key",
                &format!("{}", peer_secret_key.display_secret()),
                "--token",
                &self.wallet_token.clone().to_string(),
            ],
        );

        // in case it can't connect wait for it to crash
        std::thread::sleep(Duration::from_secs_f32(0.1));

        // status is Some if peerd returns because it crashed
        let (child, status) = child.and_then(|mut c| c.try_wait().map(|s| (c, s)))?;
        if status.is_some() {
            return Err(Error::Peer(internet2::presentation::Error::InvalidEndpoint));
        }

        self.listens.insert(bind_addr);
        debug!("New instance of peerd launched with PID {}", child.id());
        info!(
            "Connection daemon {} for incoming peer connections on {}",
            "listens".bright_green_bold(),
            bind_addr
        );
        Ok(node_id)
    }

    pub fn connect_peer(&mut self, node_addr: &NodeAddr) -> Result<(bool, ServiceId), Error> {
        self.services_ready()?;
        let (peer_secret_key, _) = self.peer_keys_ready()?;
        if let Some(spawning_peer) = self.spawning_services.iter().find(|service| {
            if let ServiceId::Peer(registered_node_addr) = service {
                registered_node_addr.id == node_addr.id
            } else {
                false
            }
        }) {
            warn!(
                "Already spawning a connection with remote peer {}, through a spawned connection {}, but have not received Hello from it yet.",
                node_addr.id, spawning_peer
            );
            return Ok((false, spawning_peer.clone()));
        };
        if let Some(existing_peer) = self.registered_services.iter().find(|service| {
            if let ServiceId::Peer(registered_node_addr) = service {
                registered_node_addr.id == node_addr.id
            } else {
                false
            }
        }) {
            debug!(
                "Already connected to remote peer {} through a spawned connection {}",
                node_addr.id, existing_peer
            );
            return Ok((true, existing_peer.clone()));
        }

        debug!("{} to remote peer {}", "Connecting", node_addr);

        // Start peerd
        let child = launch(
            "peerd",
            &[
                "--connect",
                &node_addr.to_string(),
                "--peer-secret-key",
                &format!("{}", peer_secret_key.display_secret()),
                "--token",
                &self.wallet_token.clone().to_string(),
            ],
        );

        // status is Some if peerd returns because it crashed
        let (child, status) = child.and_then(|mut c| c.try_wait().map(|s| (c, s)))?;

        if status.is_some() {
            return Err(Error::Peer(internet2::presentation::Error::InvalidEndpoint));
        }

        debug!("New instance of peerd launched with PID {}", child.id());

        self.spawning_services.insert(ServiceId::Peer(*node_addr));
        debug!("Awaiting for peerd to connect...");

        Ok((false, ServiceId::Peer(*node_addr)))
    }

    /// Notify(forward to) the subscribed clients still online with the given request
    fn notify_subscribed_clients(
        &mut self,
        endpoints: &mut Endpoints,
        source: &ServiceId,
        request: InfoMsg,
    ) {
        // if subs exists for the source (swap_id), forward the request to every subs
        if let Some(subs) = self.progress_subscriptions.get_mut(source) {
            // if the sub is no longer reachable, i.e. the process terminated without calling
            // unsub, remove it from sub list
            subs.retain(|sub| {
                endpoints
                    .send_to(
                        ServiceBus::Info,
                        ServiceId::Farcasterd,
                        sub.clone(),
                        BusMsg::Info(request.clone()),
                    )
                    .is_ok()
            });
        }
    }
}

pub fn syncer_up(
    spawning_services: &mut HashSet<ServiceId>,
    registered_services: &mut HashSet<ServiceId>,
    blockchain: Blockchain,
    network: Network,
    config: &Config,
) -> Result<Option<ServiceId>, Error> {
    let syncer_service = ServiceId::Syncer(blockchain, network);
    if !registered_services.contains(&syncer_service)
        && !spawning_services.contains(&syncer_service)
    {
        let mut args = vec![
            "--blockchain".to_string(),
            blockchain.to_string(),
            "--network".to_string(),
            network.to_string(),
        ];
        args.append(&mut syncer_servers_args(config, blockchain, network)?);
        debug!("launching syncer with: {:?}", args);
        launch("syncerd", args)?;
        spawning_services.insert(syncer_service.clone());
    }
    if registered_services.contains(&syncer_service) {
        Ok(Some(syncer_service))
    } else {
        Ok(None)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn launch_swapd(
    local_trade_role: TradeRole,
    public_offer: PublicOffer,
    swap_id: SwapId,
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
    debug!("Awaiting for swapd to connect...");
    Ok(msg)
}

/// Return the list of needed arguments for a syncer given a config and a network.
/// This function only register the minimal set of URLs needed for the blockchain to work.
fn syncer_servers_args(
    config: &Config,
    blockchain: Blockchain,
    net: Network,
) -> Result<Vec<String>, Error> {
    match config.get_syncer_servers(net) {
        Some(servers) => match blockchain {
            Blockchain::Bitcoin => Ok(vec![
                "--electrum-server".to_string(),
                servers.electrum_server,
            ]),
            Blockchain::Monero => {
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

    if let Some(d) = &matches.value_of("data-dir") {
        cmd.args(&["-d", d]);
    }

    if let Some(m) = &matches.value_of("msg-socket") {
        cmd.args(&["-m", m]);
    }

    if let Some(x) = &matches.value_of("ctl-socket") {
        cmd.args(&["-x", x]);
    }

    if let Some(i) = &matches.value_of("info-socket") {
        cmd.args(&["-i", i]);
    }

    if let Some(s) = &matches.value_of("sync-socket") {
        cmd.args(&["-S", s]);
    }

    // Forward tor proxy argument
    let parsed = Opts::parse();
    debug!("tor opts: {:?}", parsed.shared.tor_proxy);
    if let Some(t) = &matches.value_of("tor-proxy") {
        cmd.args(&["-T", *t]);
    }

    // Given specialized args in launch
    cmd.args(args);

    debug!("Executing `{:?}`", cmd);
    cmd.spawn().map_err(|err| {
        error!("Error launching {}: {}", name, err);
        err
    })
}
