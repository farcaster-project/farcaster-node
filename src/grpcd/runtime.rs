use crate::internet2::Duplex;
use crate::internet2::Encrypt;
use crate::service::Endpoints;
use internet2::zmqsocket::Connection;
use internet2::PlainTranscoder;
use std::sync::Arc;
use std::{
    any::Any,
    collections::{HashMap, HashSet},
    convert::{TryFrom, TryInto},
    io::{self, Write},
    ptr::swap_nonoverlapping,
    str::FromStr,
};
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

pub fn run(config: ServiceConfig) -> Result<(), Error> {
    let (tx_response, rx_response): (Sender<Request>, Receiver<Request>) =
        std::sync::mpsc::channel();

    let tx_request = ZMQ_CONTEXT.socket(zmq::PAIR)?;
    let rx_request = ZMQ_CONTEXT.socket(zmq::PAIR)?;
    tx_request.connect("inproc://grpcdbridge")?;
    rx_request.bind("inproc://grpcdbridge")?;

    let mut server = GrpcServer {};
    server.run(rx_response, tx_request)?;

    let runtime = Runtime {
        identity: ServiceId::Grpcd,
        server,
        tx_response,
    };

    let mut service = Service::service(config, runtime)?;
    service.add_loopback(rx_request)?;
    service.run_loop()?;
    unreachable!()
}

use farcaster::farcaster_server::{Farcaster, FarcasterServer};
use farcaster::{MakeSwapRequest, MakeSwapResponse};
use tonic::{transport::Server, Request as GrpcRequest, Response as GrpcResponse, Status};

pub mod farcaster {
    tonic::include_proto!("farcaster");
}

pub struct FarcasterService {
    tokio_tx_request: tokio::sync::mpsc::Sender<Request>,
    pending_requests: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Request>>>>,
}

#[tonic::async_trait]
impl Farcaster for FarcasterService {
    async fn make_swap(
        &self,
        request: GrpcRequest<MakeSwapRequest>,
    ) -> Result<GrpcResponse<MakeSwapResponse>, Status> {
        println!("Got a request: {:?}", request);
        let reply = farcaster::MakeSwapResponse { id: 10 };

        self.tokio_tx_request.send(Request::GetInfo).await;

        let (oneshot_tx, oneshot_rx) = tokio::sync::oneshot::channel::<Request>();
        let pending_requests = self.pending_requests.lock().await;
        pending_requests.insert(0, oneshot_tx);
        drop(pending_requests);
        let response = oneshot_rx.await;

        Ok(GrpcResponse::new(reply))
    }
}

pub struct GrpcServer {}

fn request_loop(
    tokio_rx_request: tokio::sync::mpsc::Receiver<Request>,
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
                .expect("failed to send from bitcoin syncer to syncerd bridge");
        }
    })
}

fn response_loop(
    mpsc_rx_response: Receiver<Request>,
    pending_requests: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Request>>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        loop {
            let response = mpsc_rx_response.try_recv();
            if let Ok(response) = response {
                // match response id to pending requests and one shot!
                continue;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
}

impl GrpcServer {
    fn run(
        &mut self,
        rx_response: Receiver<Request>,
        tx_request: zmq::Socket,
    ) -> Result<(), Error> {
        std::thread::spawn(move || {
            use tokio::runtime::Builder;
            let rt = Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let addr = "[::1]:50051"
                    .parse()
                    .expect("invalid grpc server bind address");
                let (tokio_tx_request, tokio_rx_request) = tokio::sync::mpsc::channel(1000);

                let pending_requests: Arc<
                    Mutex<HashMap<u64, tokio::sync::oneshot::Sender<Request>>>,
                > = Arc::new(Mutex::new(map![]));
                let request_handle = request_loop(tokio_rx_request, tx_request);
                let response_handle = response_loop(rx_response, Arc::clone(&pending_requests));

                let server = FarcasterService {
                    tokio_tx_request,
                    pending_requests,
                };

                let server_handle = Server::builder()
                    .add_service(FarcasterServer::new(server))
                    .serve(addr);

                let res = tokio::try_join!(request_handle, server_handle);
                warn!("exiting grpc server run routine with: {:?}", res);
            });
        });

        Ok(())
    }
}

pub struct Runtime {
    identity: ServiceId,
    tx_response: Sender<Request>,
    server: GrpcServer,
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
    fn send_farcasterd(
        &self,
        endpoints: &mut Endpoints,
        message: request::Request,
    ) -> Result<(), Error> {
        endpoints.send_to(
            ServiceBus::Ctl,
            self.identity(),
            ServiceId::Farcasterd,
            message,
        )?;
        Ok(())
    }

    fn handle_rpc_msg(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
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
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        match request {
            Request::Hello => match &source {
                source => {
                    debug!("Received Hello from {}", source);
                }
            },
            _ => {
                error!("Request is not supported by the CTL interface");
            }
        }
        Ok(())
    }
}
