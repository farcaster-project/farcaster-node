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
use lnp::p2p::legacy::{Messages, Ping};
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
    local_id: PublicKey,
    remote_node_addr: Option<RemoteNodeAddr>,
    local_socket: Option<InetSocketAddr>,
    remote_socket: InetSocketAddr,
    local_node: LocalNode,
    connect: bool,
) -> Result<(), Error> {
    debug!("Splitting connection into receiver and sender parts");
    let (mut peer_receiver, mut peer_sender) = connection.split();

    // this is hella hacky, but it serves the purpose of keeping peerd's service
    // id constant accross reconnects: <REMOTE_NODE_ID>:<REMOTE_ADDR> for taker,
    // <REMOTE_NODE_ID>:<LOCAL_ADDR> for maker
    // TODO: It is privacy/security critical that once the
    // connection is encrypted, this should be replaced by a proper handshake.
    let internal_identity = if connect {
        peer_sender.send_message(Msg::Identity(local_id)).unwrap();
        debug!("sent message with local_id {} to the maker", local_id);
        ServiceId::Peer(NodeAddr::Remote(RemoteNodeAddr {
            node_id: remote_node_addr.clone().unwrap().node_id,
            remote_addr: RemoteSocketAddr::Ftcp(remote_socket),
        }))
    } else {
        let unmarshaller: Unmarshaller<Msg> = Msg::create_unmarshaller();
        let msg: &Msg = &*peer_receiver.recv_message(&unmarshaller).unwrap();
        let id = match msg {
            Msg::Identity(id) => {
                info!("Received the following local_id from the taker {}", id);
                Some(id)
            }
            _ => None,
        };
        ServiceId::Peer(NodeAddr::Remote(RemoteNodeAddr {
            node_id: *id.unwrap(),
            remote_addr: RemoteSocketAddr::Ftcp(local_socket.unwrap()),
        }))
    };

    debug!("Opening bridge between runtime and peer receiver threads");
    let tx = ZMQ_CONTEXT.socket(zmq::PAIR)?;
    let rx = ZMQ_CONTEXT.socket(zmq::PAIR)?;
    tx.connect("inproc://bridge")?;
    rx.bind("inproc://bridge")?;

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
    };
    let unmarshaller: Unmarshaller<Msg> = Msg::create_unmarshaller();
    let peer_receiver_runtime = peer::Listener::<PeerReceiverRuntime, Msg>::with(
        peer_receiver,
        bridge_handler,
        unmarshaller,
    );
    // We use the _thread_flag_rx to determine when the thread had terminated
    spawn(move || peer_receiver_runtime.try_run_loop().unwrap_or(()));

    debug!(
        "Starting main service runtime with identity: {}",
        internal_identity
    );
    let runtime = Runtime {
        identity: internal_identity,
        local_id,
        remote_node_addr,
        local_socket,
        remote_socket,
        local_node,
        routing: empty!(),
        peer_sender,
        connect,
        started: SystemTime::now(),
        messages_sent: 0,
        messages_received: 0,
        awaited_pong: None,
        thread_flag_tx,
    };
    let mut service = Service::service(config, runtime)?;
    service.add_service_bus(rx)?;
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
    _thread_flag_rx: std::sync::mpsc::Receiver<()>,
}

impl PeerReceiverRuntime {
    /// send msgs over bridge from remote to local runtime
    fn send_over_bridge(
        &mut self,
        req: <Unmarshaller<Msg> as Unmarshall>::Data,
    ) -> Result<(), Error> {
        debug!("Forwarding FWP message over BRIDGE interface to the runtime");
        self.bridge.send_to(
            ServiceBus::Bridge,
            self.internal_identity.clone(),
            Request::Protocol((&*req).clone()),
        )?;
        Ok(())
    }
}

use std::fmt::{Debug, Display};
impl peer::Handler<Msg> for PeerReceiverRuntime {
    type Error = crate::Error;
    fn handle(
        &mut self,
        message: <Unmarshaller<Msg> as Unmarshall>::Data,
    ) -> Result<(), Self::Error> {
        // Forwarding all received messages to the runtime
        trace!("FWP message details: {:?}", message);
        self.send_over_bridge(message)
    }

    fn handle_err(&mut self, err: Self::Error) -> Result<(), Self::Error> {
        debug!("Underlying peer interface requested to handle {}", err);
        match err {
            Error::Peer(presentation::Error::Transport(transport::Error::TimedOut)) => {
                trace!("Time to ping the remote peer");
                // This means socket reading timeout and the fact that we need
                // to send a ping message
                self.send_over_bridge(Arc::new(Msg::PingPeer))?;
                Ok(())
            }
            Error::Peer(presentation::Error::Transport(transport::Error::SocketIo(
                std::io::ErrorKind::UnexpectedEof,
            ))) => {
                error!(
                    "The remote peer has hung up, notifying that peerd has halted: {}",
                    err
                );
                self.send_over_bridge(Arc::new(Msg::PeerReceiverRuntimeShutdown))?;
                Err(err)
            }
            // for all other error types, indicating internal errors, we
            // propagate error to the upper level (currently not handled, will
            // result in a broken peerd state)
            _ => {
                error!("Unrecoverable peer error {}, halting", err);
                Err(err)
            }
        }
    }
}

pub struct Runtime {
    identity: ServiceId,
    local_id: PublicKey,
    remote_node_addr: Option<RemoteNodeAddr>,
    local_socket: Option<InetSocketAddr>,
    remote_socket: InetSocketAddr,
    local_node: LocalNode,

    routing: HashMap<ServiceId, ServiceId>,
    peer_sender: PeerSender,
    connect: bool,

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
        if self.connect {
            info!(
                "{} with the remote peer {}",
                "Initializing connection".bright_blue_bold(),
                self.remote_socket
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

    fn handle_err(&mut self, _: &mut Endpoints, _: esb::Error<ServiceId>) -> Result<(), Error> {
        // We do nothing and do not propagate error; it's already being reported
        // with `error!` macro by the controller. If we propagate error here
        // this will make whole daemon panic
        Ok(())
    }
}

impl Runtime {
    /// send messages over the bridge
    fn handle_rpc_msg(
        &mut self,
        _endpoints: &mut Endpoints,
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
                self.peer_sender.send_message(message)?;
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
            Request::UpdateSwapId(channel_id) => {
                debug!(
                    "Renaming swapd service from temporary id {:#} to swap id #{:#}",
                    source, channel_id
                );
                self.routing.remove(&source);
                self.routing.insert(channel_id.into(), source);
            }
            Request::Terminate if source == ServiceId::Farcasterd => {
                info!("Terminating {}", self.identity().bright_white_bold());
                std::process::exit(0);
            }

            Request::GetInfo => {
                let info = PeerInfo {
                    local_id: self.local_id,
                    remote_id: self
                        .remote_node_addr
                        .clone()
                        .map(|addr| vec![addr.node_id])
                        .unwrap_or_default(),
                    local_socket: self.local_socket,
                    remote_socket: vec![self.remote_socket],
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
                    connected: !self.connect,
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

            Request::Protocol(Msg::Ping(Ping { pong_size, .. })) => self.pong(*pong_size)?,

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
                // If this is the listener-forked peerd, terminate it.
                if !self.connect {
                    endpoints.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        ServiceId::Farcasterd,
                        Request::PeerdTerminated,
                    )?;
                    warn!("Exiting peerd");
                    std::process::exit(0);
                }
                info!("Attempting to reconnect to remote peerd");
                // The PeerReceiverRuntime failed, attempt to reconnect with the counterpary
                // It is safe to unwrap remote_node_addr here, since it is Some(..) if connect=true
                let mut connection = PeerConnection::connect(
                    self.remote_node_addr.clone().unwrap(),
                    &self.local_node,
                );
                let mut attempt = 0;
                while connection.is_err() {
                    attempt += 1;
                    warn!("reconnect failed attempting again in {} seconds", attempt);
                    std::thread::sleep(std::time::Duration::from_secs(attempt));
                    connection = PeerConnection::connect(
                        self.remote_node_addr.clone().unwrap(),
                        &self.local_node,
                    );
                }
                let (peer_receiver, peer_sender) = connection.unwrap().split();
                self.peer_sender = peer_sender;
                // send the local id to the maker(listener) again
                self.peer_sender
                    .send_message(Msg::Identity(self.local_id))
                    .unwrap();

                let identity = self.identity.clone();
                let old_thread_flag_tx = self.thread_flag_tx.clone();
                let (thread_flag_tx, thread_flag_rx) = std::sync::mpsc::channel();
                info!("Peerd reconnect successfull, launching peerd receiver runtime");
                spawn(move || {
                    restart_receiver_runtime(
                        identity,
                        peer_receiver,
                        old_thread_flag_tx,
                        thread_flag_rx,
                    )
                    .unwrap_or(())
                });
                self.thread_flag_tx = thread_flag_tx;
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
        if self.awaited_pong.is_some() {
            return Err(Error::NotResponding);
        }
        let mut rng = rand::thread_rng();
        let len: u16 = rng.gen_range(4, 32);
        let mut noise = vec![0u8; len as usize];
        rng.fill_bytes(&mut noise);
        let pong_size = rng.gen_range(4, 32);
        self.messages_sent += 1;
        self.peer_sender.send_message(Msg::Ping(message::Ping {
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
    thread_flag_tx: std::sync::mpsc::Sender<()>,
    _thread_flag_rx: std::sync::mpsc::Receiver<()>,
) -> Result<(), Error> {
    // flag_rx on the old receiver thread goes out of scope, thus making
    // the send fail as soon as the old receiver thread exited.
    while thread_flag_tx.send(()).is_ok() {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let tx = ZMQ_CONTEXT.socket(zmq::PAIR)?;
    tx.connect("inproc://bridge")?;

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
    };
    let unmarshaller: Unmarshaller<Msg> = Msg::create_unmarshaller();
    let peer_receiver_runtime = peer::Listener::<PeerReceiverRuntime, Msg>::with(
        peer_receiver,
        bridge_handler,
        unmarshaller,
    );
    info!("entering peerd receiver runtime loop");
    peer_receiver_runtime.try_run_loop()
}
