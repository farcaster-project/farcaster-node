use crate::internet2::Duplex;
use crate::internet2::Encrypt;
use crate::service::Endpoints;
use internet2::zmqsocket::Connection;
use internet2::PlainTranscoder;
use std::net::SocketAddr;
use std::sync::Arc;
use std::{
    any::Any,
    collections::{HashMap, HashSet},
    convert::{TryFrom, TryInto},
    io::{self, Write},
    ptr::swap_nonoverlapping,
    str::FromStr,
};
use tokio::runtime::Builder;
use tokio::sync::Mutex;

use crate::rpc::{
    request::{self},
    Request, ServiceBus,
};
use crate::swapd::get_swap_id;
use crate::walletd::NodeSecrets;
use crate::LogStyle;
use crate::{CtlServer, Error, Service, ServiceConfig, ServiceId};
use colored::Colorize;
use internet2::{
    presentation, transport, zmqsocket, NodeAddr, RemoteSocketAddr, TypedEnum, ZmqType, ZMQ_CONTEXT,
};
use microservices::esb::{self, Handler};
use request::{LaunchSwap, NodeId};
use std::sync::mpsc::{Receiver, Sender};

use farcaster::farcaster_server::{Farcaster, FarcasterServer};
use farcaster::{InfoRequest, InfoResponse};
use tonic::{transport::Server, Request as GrpcRequest, Response as GrpcResponse, Status};

pub mod farcaster {
    tonic::include_proto!("farcaster");
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, PartialOrd, Hash, Display)]
#[display(Debug)]
pub struct IdCounter(u64);

impl IdCounter {
    fn increment(&mut self) -> u64 {
        self.0 += 1;
        self.0
    }
}

pub fn run(config: ServiceConfig, grpc_port: u64) -> Result<(), Error> {
    let (tx_response, rx_response): (Sender<Request>, Receiver<Request>) =
        std::sync::mpsc::channel();

    let tx_request = ZMQ_CONTEXT.socket(zmq::PAIR)?;
    let rx_request = ZMQ_CONTEXT.socket(zmq::PAIR)?;
    tx_request.connect("inproc://grpcdbridge")?;
    rx_request.bind("inproc://grpcdbridge")?;

    let mut server = GrpcServer { grpc_port };
    server.run(rx_response, tx_request)?;

    let runtime = Runtime {
        identity: ServiceId::Grpcd,
        tx_response,
    };

    let mut service = Service::service(config, runtime)?;
    service.add_bridge_service_bus(rx_request)?;
    service.run_loop()?;
    unreachable!()
}

pub struct FarcasterService {
    tokio_tx_request: tokio::sync::mpsc::Sender<Request>,
    pending_requests: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Request>>>>,
    id_counter: Arc<Mutex<IdCounter>>,
}

#[tonic::async_trait]
impl Farcaster for FarcasterService {
    async fn info(
        &self,
        request: GrpcRequest<InfoRequest>,
    ) -> Result<GrpcResponse<InfoResponse>, Status> {
        println!("Got a request: {:?}", request);

        let mut id_counter = self.id_counter.lock().await;
        let id = id_counter.increment();
        drop(id_counter);

        if let Err(error) = self.tokio_tx_request.send(Request::GetInfo(Some(id))).await {
            return Err(Status::internal(format!("{}", error)));
        }

        let (oneshot_tx, oneshot_rx) = tokio::sync::oneshot::channel::<Request>();
        let mut pending_requests = self.pending_requests.lock().await;
        pending_requests.insert(id, oneshot_tx);
        drop(pending_requests);
        match oneshot_rx.await {
            Ok(Request::NodeInfo(info)) => {
                let reply = farcaster::InfoResponse {
                    id: request.into_inner().id,
                    node_ids: info
                        .node_ids
                        .iter()
                        .map(|node_id| format!("{}", node_id))
                        .collect(),
                    listens: info
                        .listens
                        .iter()
                        .map(|listen| format!("{}", listen))
                        .collect(),
                    uptime: info.uptime.as_secs(),
                    since: info.since,
                    peers: info.peers.iter().map(|peer| format!("{}", peer)).collect(),
                    swaps: info.swaps.iter().map(|swap| format!("{}", swap)).collect(),
                    offers: info
                        .offers
                        .iter()
                        .map(|offer| format!("{}", offer))
                        .collect(),
                };
                Ok(GrpcResponse::new(reply))
            }
            Err(error) => Err(Status::internal(format!("{}", error))),
            _ => Err(Status::invalid_argument("received invalid response")),
        }
    }
}

pub struct GrpcServer {
    grpc_port: u64,
}

fn request_loop(
    mut tokio_rx_request: tokio::sync::mpsc::Receiver<Request>,
    tx_request: zmq::Socket,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        let mut connection = Connection::from_zmq_socket(ZmqType::Push, tx_request);
        while let Some(request) = tokio_rx_request.recv().await {
            let mut transcoder = PlainTranscoder {};
            let writer = connection.as_sender();
            debug!("sending request over syncerd bridge: {:?}", request);
            let syncer_address: Vec<u8> = ServiceId::Grpcd.into();

            writer
                .send_routed(
                    &syncer_address,
                    &syncer_address,
                    &syncer_address,
                    &transcoder.encrypt(request.serialize()),
                )
                .expect("failed to send from grpc server to grpc runtime over bridge");
        }
    })
}

fn response_loop(
    mpsc_rx_response: Receiver<Request>,
    pending_requests_lock: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Request>>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        loop {
            let response = mpsc_rx_response.try_recv();
            match response {
                Ok(Request::NodeInfo(request::NodeInfo { id: Some(id), .. })) => {
                    let mut pending_requests = pending_requests_lock.lock().await;
                    if pending_requests.contains_key(&id) {
                        let sender = pending_requests.remove(&id).unwrap();
                        sender.send(response.clone().unwrap()).expect(
                            "unable to send response from grpc resposne loop to its handler",
                        );
                    } else {
                        error!("id {} not found in pending grpc requests", id);
                    }
                }
                Ok(response) => {
                    error!(
                        "received unexpected response in grpc response loop: {:?}",
                        response
                    );
                }
                _ => {}
            }

            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
}

fn server_loop(service: FarcasterService, addr: SocketAddr) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        Server::builder()
            .add_service(FarcasterServer::new(service))
            .serve(addr)
            .await
            .expect("error running grpc server");
    })
}

impl GrpcServer {
    fn run(
        &mut self,
        rx_response: Receiver<Request>,
        tx_request: zmq::Socket,
    ) -> Result<(), Error> {
        let addr = format!("[::1]:{}", self.grpc_port)
            .parse()
            .expect("invalid grpc server bind address");

        std::thread::spawn(move || {
            let rt = Builder::new_multi_thread()
                .worker_threads(3)
                .enable_all()
                .build()
                .expect("failed to build new tokio runtime");
            rt.block_on(async {
                let (tokio_tx_request, tokio_rx_request) = tokio::sync::mpsc::channel(1000);

                let pending_requests: Arc<
                    Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Request>>>,
                > = Arc::new(Mutex::new(map![]));
                let request_handle = request_loop(tokio_rx_request, tx_request);
                let response_handle = response_loop(rx_response, Arc::clone(&pending_requests));

                let service = FarcasterService {
                    id_counter: Arc::new(Mutex::new(IdCounter(0))),
                    tokio_tx_request,
                    pending_requests,
                };

                let server_handle = server_loop(service, addr);

                // this drives the tokio execution
                let res = tokio::try_join!(request_handle, response_handle, server_handle);
                warn!("exiting grpc server run routine with: {:?}", res);
            });
        });

        Ok(())
    }
}

pub struct Runtime {
    identity: ServiceId,
    tx_response: Sender<Request>,
}

impl CtlServer for Runtime {}

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
    fn handle_rpc_msg(
        &mut self,
        _endpoints: &mut Endpoints,
        _source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        match request {
            Request::Hello => {
                // Ignoring; this is used to set remote identity at ZMQ level
            }
            _ => {
                error!("Request is not supported by the MSG interface");
            }
        }
        Ok(())
    }

    fn handle_rpc_ctl(
        &mut self,
        _endpoints: &mut Endpoints,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        match request {
            Request::Hello => {
                debug!("Received Hello from {}", source);
            }
            _ => {
                self.tx_response
                    .send(request)
                    .expect("could not send response from grpc runtime to server");
            }
        }
        Ok(())
    }

    fn handle_bridge(
        &mut self,
        endpoints: &mut Endpoints,
        _source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        debug!("GRPCD BRIDGE RPC request: {}", request);
        endpoints.send_to(
            ServiceBus::Ctl,
            self.identity(),
            ServiceId::Farcasterd,
            request,
        )?;
        Ok(())
    }
}
