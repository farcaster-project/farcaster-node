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
    message, ChannelId as SwapId, Messages, TempChannelId as TempSwapId,
    LIGHTNING_P2P_DEFAULT_PORT,
};
use lnpbp::Chain;
use microservices::esb::{self, Handler};
use microservices::rpc::Failure;

use crate::rpc::request::{IntoProgressOrFalure, NodeInfo, OptionDetails, ProtocolMessages};
use crate::rpc::{request, Request, ServiceBus};
use crate::{Config, Error, LogStyle, Service, ServiceId};

use farcaster_chains::{
    bitcoin::Bitcoin, monero::Monero, pairs::btcxmr::BtcXmr,
};
use farcaster_core::{
    blockchain::FeePolitic,
    bundle::BobParameters,
    crypto::FromSeed,
    datum::Key,
    negotiation::PublicOffer,
    protocol_message::{CommitAliceParameters, CommitBobParameters},
    role::{Alice, Bob, SwapRole},
};

use std::str::FromStr;

pub fn run(
    config: Config,
    node_id: secp256k1::PublicKey,
    seed: [u8; 32],
) -> Result<(), Error> {
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
        offers: none!(),
        seed,
    };

    Service::run(config, runtime, true)
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
    offers: HashSet<PublicOffer<BtcXmr>>,
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
            _ => {
                Err(Error::NotSupported(ServiceBus::Bridge, request.get_type()))
            }
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
        _senders: &mut esb::SenderList<ServiceBus, ServiceId>,
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
            Request::ProtocolMessages(ProtocolMessages::TakerCommit(commitment)) => {
                // FIXME needs offer id
            }
            Request::PeerMessage(_) => {
                // Ignore the rest of LN peer messages
            }

            _ => {
                error!(
                    "MSG RPC can be only used for forwarding LNPWP messages"
                );
                return Err(Error::NotSupported(
                    ServiceBus::Msg,
                    request.get_type(),
                ));
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
                            "Unexpected another farcasterd instance connection"
                                .err()
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
                        Request::Progress(format!(
                            "Swap daemon {} operational",
                            source
                        )),
                    ));
                    senders.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        source.clone(),
                        Request::TakeSwap(swap_params.clone()),
                    )?;
                    self.making_swaps.remove(&source);
                } else if let Some(swap_params) =
                    self.taking_swaps.get(&source)
                {
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
                    self.taking_swaps.remove(&source);
                } else if let Some(enquirer) =
                    self.spawning_services.get(&source)
                {
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

            Request::UpdateSwapId(new_id) => {
                debug!(
                    "Requested to update channel id {} on {}",
                    source, new_id
                );
                if let ServiceId::Swap(old_id) = source {
                    if !self.running_swaps.remove(&old_id) {
                        warn!("Swap daemon {} was unknown", source);
                    }
                    self.running_swaps.insert(new_id);
                    debug!("Registered swap daemon id {}", new_id);
                } else {
                    error!(
                        "Swap id update may be requested only by a swapd, not {}",
                        source
                    );
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
                        offers: self.offers.iter().cloned().collect(),
                    }),
                )?;
            }

            Request::ListPeers => {
                senders.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Farcasterd, // source
                    source,                // destination
                    Request::PeerList(
                        self.connections.iter().cloned().collect(),
                    ),
                )?;
            }

            Request::ListSwaps => {
                senders.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Farcasterd, // source
                    source,                // destination
                    Request::SwapList(
                        self.running_swaps.iter().cloned().collect(),
                    ),
                )?;
            }

            Request::Listen(addr) => {
                let addr_str = addr.addr();
                if self.listens.contains(&addr) {
                    let msg = format!(
                        "Listener on {} already exists, ignoring request",
                        addr
                    );
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
                        Ok(_) => info!("Connection daemon {} for incoming LN peer connections on {}", 
                                       "listens".ended(), addr_str),
                        Err(ref err) => error!("{}", err.err())
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
                notify_cli.push((
                    Some(source.clone()),
                    resp.into_progress_or_failure(),
                ));
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
            Request::MakeOffer(request::ProtoPublicOffer {
                offer,
                remote_addr,
            }) => {
                if !self.listens.contains(&remote_addr) {
                    self.listens.insert(remote_addr);
                    info!(
                        "{} for incoming LN peer connections on {}",
                        "Starting listener".promo(),
                        remote_addr.promo()
                    );
                    let resp = self.listen(remote_addr);
                    match resp {
                        Ok(_) => info!("Connection daemon {} for incoming LN peer connections on {}",
                                       "listens".ended(), remote_addr),
                        Err(ref err) => error!("{}", err.err())
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
                    notify_cli
                        .push((Some(source.clone()), Request::Progress(msg)));
                }
                let peer = internet2::RemoteNodeAddr {
                    node_id: self.node_id,
                    remote_addr,
                };
                let public_offer = offer.to_public_v1(peer);
                let hex_public_offer = public_offer.to_string();
                if self.offers.insert(public_offer) {
                    let msg = format!(
                        "{} {}",
                        "Pubic offer registered, please share with taker: "
                            .promo(),
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
                if self.offers.contains(&public_offer) {
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
                        .ok_or_else(|| {
                            internet2::presentation::Error::InvalidEndpoint
                        })?;

                    // Connect
                    let peer_connected_is_ok =
                        if !self.connections.contains(&peer) {
                            info!(
                                "{} to remote peer {}",
                                "Connecting".promo(),
                                peer.promoter()
                            );
                            let peer_connected =
                                self.connect_peer(source.clone(), peer.clone());

                            let peer_connected_is_ok = peer_connected.is_ok();

                            notify_cli.push((
                                Some(source.clone()),
                                peer_connected.into_progress_or_failure(),
                            ));
                            peer_connected_is_ok
                        } else {
                            let msg = format!(
                                "Already connected to remote peer {}",
                                peer.promoter()
                            );
                            warn!("{}", &msg);

                            notify_cli.push((
                                Some(source.clone()),
                                Request::Progress(msg),
                            ));
                            true
                        };

                    if peer_connected_is_ok {
                        let offer_registered = format!(
                            "{} {}",
                            "Pubic offer registered:".promo(),
                            &public_offer.amount()
                        );
                        // not yet in the set
                        self.offers.insert(public_offer.clone());
                        info!("{}", offer_registered.amount());

                        notify_cli.push((
                            Some(source.clone()),
                            Request::Success(OptionDetails(Some(
                                offer_registered,
                            ))),
                        ));
                    }

                    let maker = false;
                    // since we're takers, we are on the other side
                    let taker_role = offer.maker_role.other();
                    match taker_role {
                        SwapRole::Bob => {
                            let address = bitcoin::Address::from_str("bc1qesgvtyx9y6lax0x34napc2m7t5zdq6s7xxwpvk")
                                .expect("Parsable address");
                            let bob: Bob<BtcXmr> = Bob::new(address.into(), FeePolitic::Aggressive);
                            let params = bob.generate_parameters(&self.seed, &self.seed, &public_offer);
                            self.make_swap(peer.into(), Some(source), maker, public_offer, Params::Bob(params))?;
                        },
                        SwapRole::Alice => {
                            let address = bitcoin::Address::from_str("bc1qesgvtyx9y6lax0x34napc2m7t5zdq6s7xxwpvk")
                                .expect("Parsable address");
                            let alice: Alice<BtcXmr> = Alice::new(address.into(), FeePolitic::Aggressive);
                            let params = alice.generate_parameters(&self.seed, &self.seed, &public_offer);
                            self.make_swap(peer.into(), Some(source), maker, public_offer, Params::Alice(params))?;
                        }
                    };
                }
            }

            Request::Init(_) => {}
            Request::Error(_) => {}
            Request::Ping(_) => {}
            Request::Pong(_) => {}
            Request::PeerMessage(_) => {}
            Request::ProtocolMessages(_) => {}
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
            Request::SwapFunding(_) => {}
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
                senders.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Farcasterd,
                    respond_to,
                    resp,
                )?;
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
        let msg =
            format!("New instance of swapd launched with PID {}", child.id());
        info!("{}", msg);
        Ok(())
    }

    fn make_swap(
        &mut self,
        peerd: ServiceId,
        report_to: Option<ServiceId>,
        maker: bool,
        offer: PublicOffer<BtcXmr>,
        params: Params,
    ) -> Result<String, Error> {
        debug!("Instantiating swapd...");

        let tmp_swap_id = TempSwapId::random();
        // // We need to initialize temporary channel id here
        // if !maker {
        //     swap_id = TempSwapId::random();
        //     debug!(
        //         "Generated {} as a temporary channel id",
        //         swap_id
        //     );
        // }

        // Start swapd
        let child = launch("swapd", &[tmp_swap_id.to_hex()])?;
        let msg =
            format!("New instance of swapd launched with PID {}", child.id());
        info!("{}", msg);

        let list = if !maker {
            &mut self.taking_swaps
        } else {
            &mut self.making_swaps
        };
        list.insert(
            ServiceId::Swap(SwapId::from_inner(
                tmp_swap_id.into_inner(),
            )),
            request::InitSwap {
                peerd,
                report_to,
                offer,
                params,
                swap_id: tmp_swap_id.into(),
            },
        );
        debug!("Awaiting for swapd to connect...");

        Ok(msg)
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
            let msg = format!(
                "New instance of peerd launched with PID {}",
                child.id()
            );
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
        node_addr: NodeAddr,
    ) -> Result<String, Error> {
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
        let (child, status) =
            child.and_then(|mut c| c.try_wait().map(|s| (c, s)))?;

        if let Some(_) = status {
            return Err(Error::Peer(
                internet2::presentation::Error::InvalidEndpoint,
            ));
        }

        let msg =
            format!("New instance of peerd launched with PID {}", child.id());
        info!("{}", msg);

        self.spawning_services
            .insert(ServiceId::Peer(node_addr), source);
        debug!("Awaiting for peerd to connect...");

        Ok(msg)
    }
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
