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

use internet2::LocalNode;
use internet2::RemoteNodeAddr;
use internet2::RemoteSocketAddr;
use microservices::peer::PeerReceiver;
use microservices::peer::RecvMessage;
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};
use std::{collections::HashMap, sync::Arc};
use std::{rc::Rc, thread::spawn};

use amplify::Bipolar;
use bitcoin::secp256k1::rand::{self, Rng, RngCore};
use bitcoin::secp256k1::PublicKey;
use internet2::{addr::InetSocketAddr, CreateUnmarshaller, Unmarshall, Unmarshaller};
use internet2::{presentation, transport, zmqsocket, NodeAddr, TypedEnum, ZmqType, ZMQ_CONTEXT};

use crate::rpc::messages::Ping;
use microservices::esb::{self, Handler};
use microservices::node::TryService;
use microservices::peer::{self, PeerConnection, PeerSender, SendMessage};

use crate::rpc::{
    request::{self, Msg, PeerInfo, TakeCommit, Token},
    Request, ServiceBus,
};
use crate::{CtlServer, Endpoints, Error, LogStyle, Service, ServiceConfig, ServiceId};

#[allow(clippy::too_many_arguments)]
pub fn run(
    config: ServiceConfig,
    connection: PeerConnection,
    remote_node_addr: Option<RemoteNodeAddr>,
    local_socket: Option<InetSocketAddr>,
    local_node: LocalNode,
    // TODO: make this an enum instead with a descriptive distinction of listening and connecting to a listener
    forked_from_listener: bool,
) -> Result<(), Error> {
    debug!("Splitting connection into receiver and sender parts");
    let (mut peer_receiver, mut peer_sender) = connection.split();

    // this is hella hacky, but it serves the purpose of keeping peerd's service
    // id constant across reconnects: <REMOTE_NODE_ID>:<REMOTE_ADDR> for taker,
    // <REMOTE_NODE_ID>:<LOCAL_ADDR> for maker
    // TODO: It is privacy/security critical that once the
    // connection is encrypted, this should be replaced by a proper handshake.
    let internal_identity = if !forked_from_listener {
        // taker's case
        peer_sender
            .send_message(Msg::Identity(local_node.node_id()))
            .expect("failed to send taker identity to maker");
        debug!(
            "sent message with local node id {} to the maker",
            local_node.node_id()
        );
        ServiceId::Peer(NodeAddr::Remote(remote_node_addr.clone().expect(
            "remote node addr should never be None in taker (connect) case",
        )))
    } else {
        // maker's case
        let unmarshaller: Unmarshaller<Msg> = Msg::create_unmarshaller();
        let msg: &Msg = &*peer_receiver
            .recv_message(&unmarshaller)
            .expect("failed to receive identity message from maker");
        let id = match msg {
            Msg::Identity(id) => {
                debug!("Received the following local node id from the taker {}", id);
                Some(id)
            }
            _ => None,
        };
        ServiceId::Peer(NodeAddr::Remote(RemoteNodeAddr {
            node_id: *id.expect("remote id should always be some in maker's case"),
            remote_addr: RemoteSocketAddr::Ftcp(local_socket.unwrap()),
        }))
    };

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
                    carrier: zmqsocket::Carrier::Socket(tx),
                    router: None,
                    queued: true,
                }
            },
            BridgeHandler,
            ZmqType::Rep,
        )?,
        _thread_flag_rx,
        awaiting_pong: false,
    };
    let unmarshaller: Unmarshaller<Msg> = Msg::create_unmarshaller();
    let peer_receiver_runtime = peer::Listener::<PeerReceiverRuntime, Msg>::with(
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
        peer_sender,
        forked_from_listener,
        started: SystemTime::now(),
        messages_sent: 0,
        messages_received: 0,
        awaited_pong: None,
        thread_flag_tx,
    };
    let mut service = Service::service(config, runtime)?;
    service.add_bridge_service_bus(rx)?;
    service.run_loop()?;
    unreachable!()
}

pub struct BridgeHandler;

impl esb::Handler<ServiceBus> for BridgeHandler {
    type Request = Request;
    type Error = Error;

    fn identity(&self) -> ServiceId {
        ServiceId::Loopback
    }

    fn handle(
        &mut self,
        _endpoints: &mut Endpoints,
        _bus: ServiceBus,
        _addr: ServiceId,
        request: Request,
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
    bridge: esb::Controller<ServiceBus, Request, BridgeHandler>,
    awaiting_pong: bool,
    _thread_flag_rx: std::sync::mpsc::Receiver<()>,
}

impl PeerReceiverRuntime {
    /// send msgs over bridge from remote to local runtime
    fn send_over_bridge(
        &mut self,
        req: <Unmarshaller<Msg> as Unmarshall>::Data,
    ) -> Result<(), Error> {
        debug!("Forwarding FWP message over BRIDGE interface to the runtime");
        if let Err(err) = self.bridge.send_to(
            ServiceBus::Bridge,
            self.internal_identity.clone(),
            Request::Protocol((&*req).clone()),
        ) {
            error!("Error sending over bridge: {}", err);
            Err(err.into())
        } else {
            Ok(())
        }
    }
}

impl peer::Handler<Msg> for PeerReceiverRuntime {
    type Error = crate::Error;
    fn handle(
        &mut self,
        message: <Unmarshaller<Msg> as Unmarshall>::Data,
    ) -> Result<(), Self::Error> {
        trace!("FWP message details: {:?}", message);
        if let Msg::Pong(_) = *Arc::clone(&message) {
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
                    self.send_over_bridge(Arc::new(Msg::PeerReceiverRuntimeShutdown))?;
                    return Err(Error::NotResponding);
                }
                // This means socket reading timeout and the fact that we need
                // to send a ping message
                self.send_over_bridge(Arc::new(Msg::PingPeer))?;
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
                self.send_over_bridge(Arc::new(Msg::PeerReceiverRuntimeShutdown))?;
                Err(err)
            }
        }
    }
}

pub struct Runtime {
    identity: ServiceId,
    remote_node_addr: Option<RemoteNodeAddr>,
    local_socket: Option<InetSocketAddr>,
    local_node: LocalNode,

    peer_sender: PeerSender,
    // TODO: make this an enum instead with a descriptive distinction of listening and connecting to a listener
    forked_from_listener: bool,

    started: SystemTime,
    messages_sent: usize,
    messages_received: usize,
    awaited_pong: Option<u16>,

    thread_flag_tx: std::sync::mpsc::Sender<()>,
}

impl CtlServer for Runtime {}

impl esb::Handler<ServiceBus> for Runtime {
    type Request = Request;
    type Error = Error;

    fn identity(&self) -> ServiceId {
        self.identity.clone()
    }

    fn on_ready(&mut self, _endpoints: &mut Endpoints) -> Result<(), Error> {
        // log iff taker
        if !self.forked_from_listener {
            info!(
                "{} with the remote peer {}",
                "Initializing connection".bright_blue_bold(),
                self.remote_node_addr
                    .clone()
                    .expect("remote node addr is never None if forked from listener")
                    .remote_addr,
            );
        }
        Ok(())
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
            ServiceBus::Bridge => self.handle_bridge(endpoints, source, request),
        }
    }

    fn handle_err(&mut self, _: &mut Endpoints, err: esb::Error<ServiceId>) -> Result<(), Error> {
        // We do nothing and do not propagate error; it's already being reported
        // with `error!` macro by the controller. If we propagate error here
        // this will make whole daemon panic
        error!("peerd runtime received an error: {}", err);
        Ok(())
    }
}

impl Runtime {
    /// send messages over the bridge
    fn handle_rpc_msg(
        &mut self,
        endpoints: &mut Endpoints,
        _source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        match request.clone() {
            Request::Protocol(message) => {
                // 1. Check permissions
                // 2. Forward to the remote peer
                debug!("Message type: {}", message.get_type());
                debug!(
                    "Forwarding peer message to the remote peer, request: {}",
                    &request.get_type()
                );
                self.messages_sent += 1;
                while let Err(err) = self.peer_sender.send_message(message.clone()) {
                    debug!("Error sending to remote peer in peerd runtime: {}", err);
                    // If this is the listener-forked peerd, i.e. the maker's peerd, terminate it.
                    if self.forked_from_listener {
                        endpoints.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            ServiceId::Farcasterd,
                            Request::PeerdTerminated,
                        )?;
                        warn!("Exiting peerd");
                        std::process::exit(0);
                    }

                    while let Err(err) = self.reconnect_peer() {
                        warn!("error during reconnection attempt: {}", err);
                    }
                }
            }
            _ => {
                error!("MSG RPC can be only used for forwarding Protocol Messages");
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
        match request {
            Request::Terminate if source == ServiceId::Farcasterd => {
                info!("Terminating {}", self.identity().bright_white_bold());
                std::process::exit(0);
            }

            Request::GetInfo(_) => {
                let info = PeerInfo {
                    local_id: self.local_node.node_id(),
                    remote_id: self
                        .remote_node_addr
                        .clone()
                        .map(|addr| vec![addr.node_id])
                        .unwrap_or_default(),
                    local_socket: self.local_socket,
                    remote_socket: self
                        .remote_node_addr
                        .clone()
                        .map(|addr| vec![addr.remote_addr.into()])
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
                self.send_ctl(endpoints, source, Request::PeerInfo(info))?;
            }

            _ => {
                error!("Request is not supported by the CTL interface");
                return Err(Error::NotSupported(ServiceBus::Ctl, request.get_type()));
            }
        }
        Ok(())
    }

    fn reconnect_peer(&mut self) -> Result<(), Error> {
        info!("Attempting to reconnect to remote peerd");
        // The PeerReceiverRuntime failed, attempt to reconnect with the counterpary
        // It is safe to unwrap remote_node_addr here, since it is Some(..) if connect=true
        let mut connection = PeerConnection::connect(
            self.remote_node_addr
                .clone()
                .expect("is some if not forked from listener"),
            &self.local_node,
        );
        let mut attempt = 0;

        while let Err(err) = connection {
            trace!("failed to re-establish tcp connection: {}", err);
            attempt += 1;
            warn!("reconnect failed attempting again in {} seconds", attempt);
            std::thread::sleep(std::time::Duration::from_secs(attempt));
            connection = PeerConnection::connect(
                self.remote_node_addr
                    .clone()
                    .expect("is some if not forked from listener"),
                &self.local_node,
            );
        }
        let (peer_receiver, peer_sender) = connection.expect("checked with is_err()").split();
        self.peer_sender = peer_sender;
        // send the local id to the maker(listener) again
        self.peer_sender
            .send_message(Msg::Identity(self.local_node.node_id()))?;

        let identity = self.identity.clone();
        let dying_thread_flag_tx = self.thread_flag_tx.clone();
        let (thread_flag_tx, thread_flag_rx) = std::sync::mpsc::channel();
        info!("Peerd reconnect successful, launching peerd receiver runtime");
        spawn(move || {
            restart_receiver_runtime(
                identity,
                peer_receiver,
                dying_thread_flag_tx,
                thread_flag_rx,
            )
        });
        self.thread_flag_tx = thread_flag_tx;
        Ok(())
    }

    /// receive messages arriving over the bridge
    fn handle_bridge(
        &mut self,
        endpoints: &mut Endpoints,
        _source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        debug!("BRIDGE RPC request: {}", request);

        if let Request::Protocol(_) = request {
            self.messages_received += 1;
        }

        match &request {
            Request::Protocol(Msg::PingPeer) => self.ping()?,

            Request::Protocol(Msg::Ping(Ping { pong_size, .. })) => {
                debug!("receiving ping, ponging back");
                self.pong(*pong_size)?
            }

            Request::Protocol(Msg::Pong(noise)) => {
                match self.awaited_pong {
                    None => error!("Unexpected pong from the remote peer"),
                    Some(len) if len as usize != noise.len() => {
                        warn!("Pong data size does not match requested with ping")
                    }
                    _ => trace!("Got pong reply, exiting pong await mode"),
                }
                self.awaited_pong = None;
            }

            Request::Protocol(Msg::PeerReceiverRuntimeShutdown) => {
                warn!("Exiting peerd receiver runtime");
                // If this is the listener-forked peerd, i.e. the maker's peerd, terminate it.
                if self.forked_from_listener {
                    endpoints.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        ServiceId::Farcasterd,
                        Request::PeerdTerminated,
                    )?;
                    warn!("Exiting peerd");
                    std::process::exit(0);
                }

                while let Err(err) = self.reconnect_peer() {
                    warn!("error during reconnection attempt: {}", err);
                }
            }

            // swap initiation message
            Request::Protocol(Msg::TakerCommit(_)) => {
                endpoints.send_to(
                    ServiceBus::Msg,
                    self.identity(),
                    ServiceId::Farcasterd,
                    request,
                )?;
            }
            Request::Protocol(msg) => {
                endpoints.send_to(
                    ServiceBus::Msg,
                    self.identity(),
                    ServiceId::Swap(msg.swap_id()),
                    request,
                )?;
            }
            other => {
                error!("Request is not supported by the BRIDGE interface");
                dbg!(other);
                return Err(Error::NotSupported(ServiceBus::Bridge, request.get_type()));
            }
        }
        Ok(())
    }

    fn ping(&mut self) -> Result<(), Error> {
        trace!("Sending ping to the remote peer");
        let mut rng = rand::thread_rng();
        let len: u16 = rng.gen_range(4, 32);
        let mut noise = vec![0u8; len as usize];
        rng.fill_bytes(&mut noise);
        let pong_size = rng.gen_range(4, 32);
        self.messages_sent += 1;
        self.peer_sender.send_message(Msg::Ping(Ping {
            ignored: noise,
            pong_size,
        }))?;
        self.awaited_pong = Some(pong_size);
        Ok(())
    }

    fn pong(&mut self, pong_size: u16) -> Result<(), Error> {
        trace!("Replying with pong to the remote peer");
        let mut rng = rand::thread_rng();
        let noise = vec![0u8; pong_size as usize]
            .iter()
            .map(|_| rng.gen())
            .collect();
        self.messages_sent += 1;
        self.peer_sender.send_message(Msg::Pong(noise))?;
        Ok(())
    }
}

fn restart_receiver_runtime(
    internal_identity: ServiceId,
    peer_receiver: PeerReceiver,
    dying_thread_flag_tx: std::sync::mpsc::Sender<()>,
    _thread_flag_rx: std::sync::mpsc::Receiver<()>,
) {
    // flag_rx on the old receiver thread goes out of scope, thus making
    // the send fail as soon as the old receiver thread exited.
    while dying_thread_flag_tx.send(()).is_ok() {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let tx = ZMQ_CONTEXT
        .socket(zmq::PUSH)
        .expect("unable to create new bridge zmq socket");
    tx.connect("inproc://bridge")
        .expect("unable to connec to zmq bridge");

    debug!("Starting thread listening for messages from the remote peer");
    let bridge_handler = PeerReceiverRuntime {
        internal_identity,
        bridge: esb::Controller::with(
            map! {
                ServiceBus::Bridge => esb::BusConfig {
                    carrier: zmqsocket::Carrier::Socket(tx),
                    router: None,
                    queued: true,
                }
            },
            BridgeHandler,
            ZmqType::Rep,
        )
        .expect("error re-creating receiver runtime bridge"),
        awaiting_pong: false,
        _thread_flag_rx,
    };
    let unmarshaller: Unmarshaller<Msg> = Msg::create_unmarshaller();
    let peer_receiver_runtime = peer::Listener::<PeerReceiverRuntime, Msg>::with(
        peer_receiver,
        bridge_handler,
        unmarshaller,
    );
    debug!("entering peerd receiver runtime loop");
    if let Err(err) = peer_receiver_runtime.try_run_loop() {
        error!(
            "Error encountered in peer receiver runtime, receiver runtime is stopped: {}",
            err
        )
    }
}
