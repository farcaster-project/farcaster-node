// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use farcaster_core::swap::SwapId;
use internet2::addr::LocalNode;
use microservices::peer::RecvMessage;
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread::spawn;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

use amplify::Bipolar;
use bitcoin::secp256k1::rand::{self, Rng, RngCore};
use internet2::{addr::InetSocketAddr, CreateUnmarshaller, Unmarshall, Unmarshaller};
use internet2::{
    addr::NodeAddr,
    presentation, transport,
    zeromq::{Carrier, ZmqSocketType},
    TypedEnum,
};
use microservices::esb::{self, Handler};
use microservices::node::TryService;
use microservices::peer::{self, PeerConnection, PeerSender, SendMessage};
use microservices::ZMQ_CONTEXT;

use crate::bus::p2p::Receipt;
use crate::bus::{
    ctl::CtlMsg,
    info::{InfoMsg, PeerInfo},
    p2p::PeerMsg,
    BusMsg, ServiceBus,
};
use crate::{CtlServer, Endpoints, Error, LogStyle, Service, ServiceConfig, ServiceId};

pub fn start_connect_peer_listener_runtime(
    remote_node_addr: NodeAddr,
    local_node: LocalNode,
    peerd_service_id: ServiceId,
    tor_proxy: Option<SocketAddr>,
) -> Result<(PeerSender, std::sync::mpsc::Sender<()>), Error> {
    if tor_proxy.is_none() && matches!(remote_node_addr.addr, InetSocketAddr::Tor(_)) {
        return Err(Error::Farcaster(format!(
            "Can only connect to remote peer on Tor address {} when Tor proxy is set too",
            remote_node_addr
        )));
    }
    let connection = PeerConnection::connect_brontozaur(local_node, remote_node_addr)?;
    debug!("Connected to remote peer: {}", remote_node_addr);

    debug!("Splitting connection into receiver and sender parts");
    let (mut peer_receiver, mut peer_sender) = connection.split();

    // this is hella hacky, but it serves the purpose of keeping peerd's service
    // id constant across reconnects: <REMOTE_NODE_ID>:<REMOTE_ADDR> for taker,
    // <REMOTE_NODE_ID>:<LOCAL_ADDR> for maker
    // TODO: It is privacy/security critical that once the
    // connection is encrypted, this should be replaced by a proper handshake.
    peer_sender.send_message(PeerMsg::Identity(local_node.node_id()))?;
    debug!(
        "sent message with local node id {} to the maker",
        local_node.node_id()
    );
    let unmarshaller: Unmarshaller<PeerMsg> = PeerMsg::create_unmarshaller();
    let msg: &PeerMsg = &*peer_receiver.recv_message(&unmarshaller)?;
    match msg {
        PeerMsg::Pong(id) => {
            debug!("Received the following pong from the maker {:?}", id);
        }
        _ => {
            return Err(Error::Peer(presentation::Error::UnknownDataType));
        }
    };

    let tx = ZMQ_CONTEXT.socket(zmq::PUSH)?;
    tx.connect("inproc://bridge")?;

    let (thread_flag_tx, _thread_flag_rx) = std::sync::mpsc::channel();

    debug!("Starting thread listening for messages from the remote peer");
    let bridge_handler = PeerReceiverRuntime {
        internal_identity: peerd_service_id,
        bridge: esb::Controller::with(
            map! {
                ServiceBus::Bridge => esb::BusConfig {
                    carrier: Carrier::Socket(tx),
                    router: None,
                    queued: true,
                    api_type: ZmqSocketType::Rep,
                    topic: None,
                }
            },
            BridgeHandler,
        )?,
        _thread_flag_rx,
        awaiting_pong: false,
    };
    let unmarshaller: Unmarshaller<PeerMsg> = PeerMsg::create_unmarshaller();
    let peer_receiver_runtime = peer::Listener::<PeerReceiverRuntime, PeerMsg>::with(
        peer_receiver,
        bridge_handler,
        unmarshaller,
    );
    // We use the _thread_flag_rx to determine when the thread had terminated
    spawn(move || {
        debug!("entering peerd receiver runtime loop");
        if let Err(err) = peer_receiver_runtime.try_run_loop() {
            error!(
                "Error encountered in peer receiver runtime, receiver runtime is stopped: {}",
                err
            );
        }
    });

    Ok((peer_sender, thread_flag_tx))
}

pub fn run_from_connect(
    config: ServiceConfig,
    remote_node_addr: NodeAddr,
    local_socket: Option<InetSocketAddr>,
    local_node: LocalNode,
    tor_proxy: Option<SocketAddr>,
) -> Result<(), Error> {
    debug!("Opening bridge between runtime and peer receiver threads");
    let rx = ZMQ_CONTEXT.socket(zmq::PULL)?;
    rx.bind("inproc://bridge")?;

    let (thread_flag_tx, _thread_flag_rx) = std::sync::mpsc::channel();
    let id = Uuid::new_v4().as_u128();

    debug!(
        "Starting main service runtime with identity: {}",
        ServiceId::Peer(id, remote_node_addr)
    );
    let runtime = Runtime {
        identity: ServiceId::Peer(id, remote_node_addr),
        remote_node_addr: Some(remote_node_addr),
        local_socket,
        local_node,
        tor_proxy,
        peer_sender: None, // As connector we create the sender on is_ready
        forked_from_listener: false,
        started: SystemTime::now(),
        messages_sent: 0,
        messages_received: 0,
        awaited_pong: None,
        thread_flag_tx,
        unchecked_msg_cache: empty!(),
    };
    let mut service = Service::service(config, runtime)?;
    service.add_bridge_service_bus(rx)?;
    service.run_loop()?;
    unreachable!()
}

#[allow(clippy::too_many_arguments)]
pub fn run_from_listener(
    config: ServiceConfig,
    connection: PeerConnection,
    remote_node_addr: Option<NodeAddr>,
    local_socket: Option<InetSocketAddr>,
    local_node: LocalNode,
) -> Result<(), Error> {
    debug!("Splitting connection into receiver and sender parts");
    let (mut peer_receiver, mut peer_sender) = connection.split();

    // this is hella hacky, but it serves the purpose of keeping peerd's service
    // id constant across reconnects: <REMOTE_NODE_ID>:<REMOTE_ADDR> for taker,
    // <REMOTE_NODE_ID>:<LOCAL_ADDR> for maker
    // TODO: It is privacy/security critical that once the
    // connection is encrypted, this should be replaced by a proper handshake.
    let unmarshaller: Unmarshaller<PeerMsg> = PeerMsg::create_unmarshaller();
    let msg: Arc<PeerMsg> = peer_receiver
        .recv_message(&unmarshaller)
        .expect("failed to receive identity message from taker");
    let id = match *msg {
        PeerMsg::Identity(id) => {
            debug!("Received the following local node id from the taker {}", id);
            Some(id)
        }
        _ => None,
    };
    peer_sender
        .send_message(PeerMsg::Pong(vec![0]))
        .expect("Failed to send handshake pong");
    let peerd_id = Uuid::new_v4().as_u128();
    let internal_identity = ServiceId::Peer(
        peerd_id,
        NodeAddr {
            id: id.expect("remote id should always be some in maker's case"),
            addr: local_socket.expect("Checked for listener"),
        },
    );

    debug!("Opening bridge between runtime and peer receiver threads");
    let rx = ZMQ_CONTEXT.socket(zmq::PULL)?;
    rx.bind("inproc://bridge")?;
    let tx = ZMQ_CONTEXT.socket(zmq::PUSH)?;
    tx.connect("inproc://bridge")?;

    let (thread_flag_tx, _thread_flag_rx) = std::sync::mpsc::channel();

    debug!("Starting thread listening for messages from the remote peer");
    let bridge_handler = PeerReceiverRuntime {
        internal_identity: internal_identity.clone(),
        bridge: esb::Controller::with(
            map! {
                ServiceBus::Bridge => esb::BusConfig {
                    carrier: Carrier::Socket(tx),
                    router: None,
                    queued: true,
                    api_type: ZmqSocketType::Rep,
                    topic: None,
                }
            },
            BridgeHandler,
        )?,
        _thread_flag_rx,
        awaiting_pong: false,
    };
    let unmarshaller: Unmarshaller<PeerMsg> = PeerMsg::create_unmarshaller();
    let peer_receiver_runtime = peer::Listener::<PeerReceiverRuntime, PeerMsg>::with(
        peer_receiver,
        bridge_handler,
        unmarshaller,
    );
    // We use the _thread_flag_rx to determine when the thread had terminated
    spawn(move || {
        if let Err(err) = peer_receiver_runtime.try_run_loop() {
            error!(
                "Error encountered in peer receiver runtime, receiver runtime is stopped: {}",
                err
            );
        }
    });

    debug!(
        "Starting main service runtime with identity: {}",
        internal_identity
    );
    let runtime = Runtime {
        identity: internal_identity,
        remote_node_addr,
        local_socket,
        local_node,
        tor_proxy: None,
        peer_sender: Some(peer_sender),
        forked_from_listener: true,
        started: SystemTime::now(),
        messages_sent: 0,
        messages_received: 0,
        awaited_pong: None,
        thread_flag_tx,
        unchecked_msg_cache: empty!(),
    };
    let mut service = Service::service(config, runtime)?;
    service.add_bridge_service_bus(rx)?;
    service.run_loop()?;
    unreachable!()
}

pub struct BridgeHandler;

impl esb::Handler<ServiceBus> for BridgeHandler {
    type Request = BusMsg;
    type Error = Error;

    fn identity(&self) -> ServiceId {
        ServiceId::Loopback
    }

    fn handle(
        &mut self,
        _endpoints: &mut Endpoints,
        _bus: ServiceBus,
        _addr: ServiceId,
        request: BusMsg,
    ) -> Result<(), Error> {
        // Bridge does not receive replies for now
        trace!("BridgeHandler received reply: {}", request);
        Ok(())
    }

    fn handle_err(&mut self, _: &mut Endpoints, err: esb::Error<ServiceId>) -> Result<(), Error> {
        // We simply propagate the error since it's already being reported
        Err(Error::Esb(err))
    }
}

// PeerReceiverRuntime handles incoming messages only
pub struct PeerReceiverRuntime {
    internal_identity: ServiceId,
    bridge: esb::Controller<ServiceBus, BusMsg, BridgeHandler>,
    awaiting_pong: bool,
    _thread_flag_rx: std::sync::mpsc::Receiver<()>,
}

impl PeerReceiverRuntime {
    /// send msgs over bridge from remote to local runtime
    fn send_over_bridge(
        &mut self,
        req: <Unmarshaller<PeerMsg> as Unmarshall>::Data,
    ) -> Result<(), Error> {
        debug!("Forwarding FWP message over BRIDGE interface to the runtime");
        if let Err(err) = self.bridge.send_to(
            ServiceBus::Bridge,
            self.internal_identity.clone(),
            BusMsg::P2p((*req).clone()),
        ) {
            error!("Error sending over bridge: {}", err);
            Err(err.into())
        } else {
            Ok(())
        }
    }
}

impl peer::Handler<PeerMsg> for PeerReceiverRuntime {
    type Error = crate::Error;

    fn handle(
        &mut self,
        message: <Unmarshaller<PeerMsg> as Unmarshall>::Data,
    ) -> Result<(), Self::Error> {
        trace!("FWP message details: {:?}", message);
        if let PeerMsg::Pong(_) = *Arc::clone(&message) {
            if self.awaiting_pong {
                self.awaiting_pong = false;
            } else {
                error!("Unexpected pong received in PeerReceiverRuntime.")
            }
        }
        if message.on_receiver_whitelist() {
            self.send_over_bridge(message)?;
        } else {
            debug!(
                "Ignoring message {}, did not match peer receiving whitelist",
                message
            );
        }
        Ok(())
    }

    fn handle_err(&mut self, err: Self::Error) -> Result<(), Self::Error> {
        debug!("Underlying peer interface requested to handle {}", err);
        match err {
            Error::Peer(presentation::Error::Transport(transport::Error::TimedOut)) => {
                trace!("Time to ping the remote peer");
                if self.awaiting_pong {
                    error!(
                        "The ping has failed, probably the connection is down. Will shutdown the receiver runtime."
                    );
                    self.send_over_bridge(Arc::new(PeerMsg::PeerReceiverRuntimeShutdown))?;
                    return Err(Error::NotResponding);
                }
                // This means socket reading timeout and the fact that we need
                // to send a ping message
                self.send_over_bridge(Arc::new(PeerMsg::PingPeer))?;
                self.awaiting_pong = true;
                Ok(())
            }
            // for all other error types, indicating internal errors and broken
            // connections, we propagate error to the upper level (currently not
            // handled, will result in a broken peerd state)
            _ => {
                error!(
                    "The remote connection is broken; notifying peerd that its receiver runtime is halting: {}",
                    err
                );
                self.send_over_bridge(Arc::new(PeerMsg::PeerReceiverRuntimeShutdown))?;
                Err(err)
            }
        }
    }
}

pub struct Runtime {
    identity: ServiceId,
    remote_node_addr: Option<NodeAddr>,
    local_socket: Option<InetSocketAddr>,
    local_node: LocalNode,
    tor_proxy: Option<SocketAddr>,

    peer_sender: Option<PeerSender>,
    // TODO: make this an enum instead with a descriptive distinction of listening and connecting to a listener
    forked_from_listener: bool,

    started: SystemTime,
    messages_sent: usize,
    messages_received: usize,
    awaited_pong: Option<u16>,

    unchecked_msg_cache: Vec<((SwapId, internet2::TypeId), PeerMsg)>,

    thread_flag_tx: std::sync::mpsc::Sender<()>,
}

impl CtlServer for Runtime {}

impl esb::Handler<ServiceBus> for Runtime {
    type Request = BusMsg;
    type Error = Error;

    fn identity(&self) -> ServiceId {
        self.identity.clone()
    }

    fn on_ready(&mut self, endpoints: &mut Endpoints) -> Result<(), Error> {
        // log iff taker
        if !self.forked_from_listener {
            let (peer_sender, thread_flag_tx) = match start_connect_peer_listener_runtime(
                self.remote_node_addr.expect("Checked for connecter"),
                self.local_node,
                self.identity(),
                self.tor_proxy,
            ) {
                Ok(val) => {
                    debug!(
                        "{} | Successfully connected to remote peer: {}",
                        self.identity(),
                        self.remote_node_addr.expect("Checked for connecter")
                    );
                    endpoints.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        ServiceId::Farcasterd,
                        BusMsg::Ctl(CtlMsg::ConnectSuccess),
                    )?;
                    val
                }
                Err(err) => {
                    error!(
                        "{} | Failed to connect to remote peer: {}, exiting",
                        self.identity(),
                        err
                    );
                    endpoints.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        ServiceId::Farcasterd,
                        BusMsg::Ctl(CtlMsg::ConnectFailed),
                    )?;
                    return Ok(());
                }
            };

            self.peer_sender = Some(peer_sender);
            self.thread_flag_tx = thread_flag_tx;
            info!(
                "{} with the remote peer {}",
                "Successfully connected".bright_blue_bold(),
                self.remote_node_addr
                    .expect("remote node addr is never None if forked from listener")
            );
        }
        Ok(())
    }

    fn handle(
        &mut self,
        endpoints: &mut Endpoints,
        bus: ServiceBus,
        source: ServiceId,
        request: BusMsg,
    ) -> Result<(), Self::Error> {
        match (bus, request) {
            // Peer-to-peer message bus, only accept BusMsg::P2p
            (ServiceBus::Msg, BusMsg::P2p(req)) => self.handle_msg(endpoints, source, req),
            // Control bus for issuing control commands, only accept BusMsg::Ctl
            (ServiceBus::Ctl, BusMsg::Ctl(req)) => self.handle_ctl(endpoints, source, req),
            // RPC command bus, only accept BusMsg::Info
            (ServiceBus::Info, BusMsg::Info(req)) => self.handle_info(endpoints, source, req),
            // Internal peerd bridge for inner communication, only accept BusMsg::P2p
            (ServiceBus::Bridge, BusMsg::P2p(req)) => self.handle_bridge(endpoints, source, req),
            // All other pairs are not supported
            (_, request) => Err(Error::NotSupported(bus, request.to_string())),
        }
    }

    fn handle_err(&mut self, _: &mut Endpoints, err: esb::Error<ServiceId>) -> Result<(), Error> {
        // We do nothing and do not propagate error; it's already being reported
        // with `error!` macro by the controller. If we propagate error here
        // this will make whole daemon panic
        error!(
            "{} | peerd runtime received an error: {}",
            self.identity(),
            err
        );
        Ok(())
    }
}

impl Runtime {
    /// send messages over the peer connection
    fn handle_msg(
        &mut self,
        endpoints: &mut Endpoints,
        _source: ServiceId,
        message: PeerMsg,
    ) -> Result<(), Error> {
        // Forward to the remote peer
        debug!("{} | Message type: {}", self.identity(), message.get_type());
        debug!(
            "{} | Forwarding peer message to the remote peer, request: {}",
            self.identity(),
            &message.get_type()
        );
        self.messages_sent += 1;
        while let Err(err) = self
            .peer_sender
            .as_mut()
            .expect("should be connected")
            .send_message(message.clone())
        {
            endpoints.send_to(
                ServiceBus::Ctl,
                self.identity(),
                ServiceId::Farcasterd,
                BusMsg::Ctl(CtlMsg::Disconnected),
            )?;
            debug!(
                "{} | Error sending to remote peer in peerd runtime: {}",
                self.identity(),
                err
            );
            // If this is the listener-forked peerd, i.e. the maker's peerd, terminate it.
            if self.forked_from_listener {
                for (_, cached_msg) in self.unchecked_msg_cache.drain(..) {
                    // Draining cached messages to the various running swaps
                    endpoints.send_to(
                        ServiceBus::Ctl,
                        self.identity.clone(),
                        ServiceId::Swap(cached_msg.swap_id()),
                        BusMsg::Ctl(CtlMsg::FailedPeerMessage(cached_msg)),
                    )?;
                }
                endpoints.send_to(
                    ServiceBus::Ctl,
                    self.identity(),
                    ServiceId::Farcasterd,
                    BusMsg::Ctl(CtlMsg::PeerdTerminated),
                )?;
                warn!(
                    "Waiting for farcaster to terminate peerd instance {}",
                    self.identity().label()
                );
                // Return here, Farcaster is supposed to terminate us
                return Ok(());
            }

            // This blocks until reconnected successfully
            while let Err(err) = self.reconnect_peer(endpoints) {
                info!(
                    "{} | Failed to reconnect: {}, retrying.",
                    self.identity(),
                    err
                );
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }

        if message.is_protocol() {
            // First remove any dangling messages with the same type and origin
            self.unchecked_msg_cache
                .iter()
                .position(|(key, _)| key.0 == message.swap_id() && key.1 == message.get_type())
                .map(|pos| self.unchecked_msg_cache.remove(pos));
            // Then push the message to the back of the vector
            self.unchecked_msg_cache
                .push(((message.swap_id(), message.get_type()), message.clone()));
            let swap_id = message.swap_id();
            info!(
                "{} | Sent the {} protocol message",
                swap_id.swap_id(),
                message.label()
            );
        }

        Ok(())
    }

    fn handle_ctl(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: CtlMsg,
    ) -> Result<(), Error> {
        match request {
            CtlMsg::Terminate if source == ServiceId::Farcasterd => {
                for (_, cached_msg) in self.unchecked_msg_cache.drain(..) {
                    // Draining cached messages to the various running swaps
                    endpoints.send_to(
                        ServiceBus::Ctl,
                        self.identity.clone(),
                        ServiceId::Swap(cached_msg.swap_id()),
                        BusMsg::Ctl(CtlMsg::FailedPeerMessage(cached_msg)),
                    )?;
                }
                // FIXME: if persist pid logging beyond debugging, make this idiomatic across all services
                info!(
                    "Terminating {} with PID {}",
                    self.identity().label(),
                    std::process::id()
                );

                std::process::exit(0);
            }

            _ => {
                error!(
                    "{} | BusMsg is not supported by the CTL interface",
                    self.identity().label()
                );
                Err(Error::NotSupported(ServiceBus::Ctl, request.to_string()))
            }
        }
    }

    fn handle_info(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: InfoMsg,
    ) -> Result<(), Error> {
        match request {
            InfoMsg::GetInfo => {
                let info = PeerInfo {
                    local_id: self.local_node.node_id(),
                    remote_id: self
                        .remote_node_addr
                        .map(|addr| vec![addr.id])
                        .unwrap_or_default(),
                    local_socket: self.local_socket,
                    remote_socket: self
                        .remote_node_addr
                        .map(|addr| vec![addr.addr])
                        .unwrap_or_default(),
                    uptime: SystemTime::now()
                        .duration_since(self.started)
                        .unwrap_or_else(|_| Duration::from_secs(0)),
                    since: self
                        .started
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap_or_else(|_| Duration::from_secs(0))
                        .as_secs(),
                    messages_sent: self.messages_sent,
                    messages_received: self.messages_received,
                    forked_from_listener: self.forked_from_listener,
                    awaits_pong: self.awaited_pong.is_some(),
                };
                self.send_client_info(endpoints, source, InfoMsg::PeerInfo(info))?;
            }

            req => {
                warn!(
                    "{} | Ignoring request: {}",
                    self.identity().label(),
                    req.err()
                );
            }
        }

        Ok(())
    }

    fn reconnect_peer(&mut self, endpoints: &mut Endpoints) -> Result<(), Error> {
        // flag_rx on the old receiver thread goes out of scope, thus making
        // the send fail as soon as the old receiver thread exited.
        while self.thread_flag_tx.send(()).is_ok() {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        let mut attempt = 0;
        loop {
            match start_connect_peer_listener_runtime(
                self.remote_node_addr.expect("Checked for connnecter"),
                self.local_node,
                self.identity(),
                self.tor_proxy,
            ) {
                Err(err) => {
                    attempt += 1;
                    trace!("{} | Failed to reconnect: {}", self.identity(), err);
                    warn!(
                        "{} | Reconnect failed attempting again in {} seconds",
                        self.identity(),
                        attempt
                    );
                    std::thread::sleep(std::time::Duration::from_secs(attempt));
                }
                Ok((peer_sender, thread_flag_tx)) => {
                    info!(
                        "{} | Reconnect success after {} attempts",
                        self.identity(),
                        attempt
                    );
                    self.peer_sender = Some(peer_sender);
                    self.thread_flag_tx = thread_flag_tx;
                    break;
                }
            }
        }
        for (_, cached_msg) in self.unchecked_msg_cache.iter() {
            info!(
                "{} | re-emitting cached message after reconnect: {}",
                cached_msg.swap_id(),
                cached_msg
            );
            self.peer_sender
                .as_mut()
                .expect("should be connected")
                .send_message(cached_msg.clone())?;
        }
        endpoints.send_to(
            ServiceBus::Ctl,
            self.identity(),
            ServiceId::Farcasterd,
            BusMsg::Ctl(CtlMsg::Reconnected),
        )?;
        Ok(())
    }

    /// receive messages arriving over the bridge
    fn handle_bridge(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: PeerMsg,
    ) -> Result<(), Error> {
        debug!("{} | BRIDGE RPC request: {}", self.identity(), request);

        self.messages_received += 1;

        match &request {
            PeerMsg::PingPeer => self.ping()?,

            PeerMsg::Ping(pong_size) => {
                debug!("{} | receiving ping, ponging back", self.identity().label());
                self.pong(*pong_size)?
            }

            PeerMsg::Pong(noise) => {
                match self.awaited_pong {
                    None => error!("{} | Unexpected pong from the remote peer", self.identity()),
                    Some(len) if len as usize != noise.len() => {
                        warn!(
                            "{} | Pong data size does not match requested with ping",
                            self.identity()
                        );
                    }
                    _ => trace!(
                        "{} | Got pong reply, exiting pong await mode",
                        self.identity()
                    ),
                }
                self.awaited_pong = None;
            }

            PeerMsg::PeerReceiverRuntimeShutdown => {
                warn!("{} | Exiting peerd receiver runtime", self.identity());
                endpoints.send_to(
                    ServiceBus::Ctl,
                    self.identity(),
                    ServiceId::Farcasterd,
                    BusMsg::Ctl(CtlMsg::Disconnected),
                )?;
                // If this is the listener-forked peerd, i.e. the maker's peerd, terminate it.
                if self.forked_from_listener {
                    endpoints.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        ServiceId::Farcasterd,
                        BusMsg::Ctl(CtlMsg::PeerdTerminated),
                    )?;
                    for ((swap_id, _), cached_msg) in self.unchecked_msg_cache.drain(..) {
                        // Draining cached messages to the various running swaps
                        debug!(
                            "{} | Returning cache message {} back to swap",
                            swap_id, cached_msg
                        );
                        endpoints.send_to(
                            ServiceBus::Ctl,
                            self.identity.clone(),
                            ServiceId::Swap(swap_id),
                            BusMsg::Ctl(CtlMsg::FailedPeerMessage(cached_msg)),
                        )?;
                    }
                    warn!(
                        "Waiting for farcaster to terminate peerd instance {}",
                        self.identity().label()
                    );
                } else {
                    // This blocks until reconnected successfully
                    while let Err(err) = self.reconnect_peer(endpoints) {
                        info!(
                            "{} | Failed to reconnect: {}, retrying.",
                            self.identity(),
                            err
                        );
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    }
                }
            }

            PeerMsg::MsgReceipt(receipt) => {
                debug!("{} | received receipt: {:?}", request.swap_id(), receipt);

                self.unchecked_msg_cache
                    .iter()
                    .position(|(key, _)| key.0 == receipt.swap_id && key.1 == receipt.msg_type)
                    .map(|pos| self.unchecked_msg_cache.remove(pos));
            }

            // swap initiation message
            PeerMsg::TakerCommit(_) => {
                let swap_id = request.swap_id();
                let msg_type = request.get_type();
                debug!(
                    "{} | PID {} | Received the {} protocol message, forwarding to farcasterd",
                    swap_id.swap_id(),
                    std::process::id(),
                    request.label()
                );
                endpoints.send_to(
                    ServiceBus::Msg,
                    self.identity(),
                    ServiceId::Farcasterd,
                    BusMsg::P2p(request),
                )?;

                // send a receipt back to the remote peer
                self.handle_msg(
                    endpoints,
                    source,
                    PeerMsg::MsgReceipt(Receipt { swap_id, msg_type }),
                )?;
            }

            msg => {
                debug_assert!(msg.is_protocol());
                let swap_id = msg.swap_id();
                debug!(
                    "{} | PID {} | Received the {} protocol message, forwarding to swapd",
                    swap_id.swap_id(),
                    std::process::id(),
                    msg.label()
                );
                endpoints.send_to(
                    ServiceBus::Msg,
                    self.identity(),
                    ServiceId::Swap(swap_id),
                    BusMsg::P2p(request.clone()),
                )?;

                // send a receipt back to the remote peer
                self.peer_sender
                    .as_mut()
                    .expect("should be connected")
                    .send_message(PeerMsg::MsgReceipt(Receipt {
                        swap_id: request.swap_id(),
                        msg_type: request.get_type(),
                    }))?;
            }
        }
        Ok(())
    }

    fn ping(&mut self) -> Result<(), Error> {
        trace!("{} | Sending ping to the remote peer", self.identity());
        let mut rng = rand::thread_rng();
        let len: u16 = rng.gen_range(4, 32);
        let mut noise = vec![0u8; len as usize];
        rng.fill_bytes(&mut noise);
        let pong_size = rng.gen_range(4, 32);
        self.messages_sent += 1;
        self.peer_sender
            .as_mut()
            .expect("should be connected")
            .send_message(PeerMsg::Ping(pong_size))?;
        self.awaited_pong = Some(pong_size);
        Ok(())
    }

    fn pong(&mut self, pong_size: u16) -> Result<(), Error> {
        trace!(
            "{} | Replying with pong to the remote peer",
            self.identity()
        );
        let mut rng = rand::thread_rng();
        let noise = vec![0u8; pong_size as usize]
            .iter()
            .map(|_| rng.gen())
            .collect();
        self.messages_sent += 1;
        self.peer_sender
            .as_mut()
            .expect("should be connected")
            .send_message(PeerMsg::Pong(noise))?;
        Ok(())
    }
}
