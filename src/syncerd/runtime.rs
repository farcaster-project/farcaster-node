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

use crate::service::Endpoints;
use crate::syncerd::bitcoin_syncer::BitcoinSyncer;
use crate::syncerd::monero_syncer::MoneroSyncer;
use crate::syncerd::opts::Opts;
use crate::syncerd::runtime::request::Progress;
use amplify::Wrapper;
use farcaster_core::blockchain::{Blockchain, Network};
use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::ffi::OsStr;
use std::io;
use std::net::SocketAddr;
use std::process;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::time::{Duration, SystemTime};

use bitcoin::hashes::hex::ToHex;
use bitcoin::secp256k1;
use farcaster_core::swap::SwapId;
use internet2::{addr::NodeAddr, presentation, transport, zeromq::ZmqSocketType, TypedEnum};
use microservices::esb::{self, Handler};
use microservices::rpc::Failure;
use microservices::ZMQ_CONTEXT;

use crate::rpc::request::{IntoProgressOrFailure, OptionDetails, SyncerInfo};
use crate::rpc::{request, Request, ServiceBus};
use crate::syncerd::*;
use crate::{Error, LogStyle, Service, ServiceConfig, ServiceId};

pub trait Synclet {
    fn run(
        &mut self,
        rx: Receiver<SyncerdTask>,
        tx: zmq::Socket,
        syncer_address: Vec<u8>,
        opts: &Opts,
        network: Network,
        polling: bool,
    ) -> Result<(), Error>;
}

pub struct SyncerdTask {
    pub task: Task,
    pub source: ServiceId,
}

pub fn run(config: ServiceConfig, opts: Opts) -> Result<(), Error> {
    let blockchain = opts.blockchain;
    let network = opts.network;

    info!("Creating new {} ({}) syncer", &blockchain, &network);
    let (tx, rx): (Sender<SyncerdTask>, Receiver<SyncerdTask>) = std::sync::mpsc::channel();

    let tx_event = ZMQ_CONTEXT.socket(zmq::PAIR)?;
    let rx_event = ZMQ_CONTEXT.socket(zmq::PAIR)?;
    rx_event.bind("inproc://syncerdbridge")?;
    tx_event.connect("inproc://syncerdbridge")?;

    let syncer: Box<dyn Synclet> = match blockchain {
        Blockchain::Monero => Box::new(MoneroSyncer::new()),
        Blockchain::Bitcoin => Box::new(BitcoinSyncer::new()),
    };

    let mut runtime = Runtime {
        identity: ServiceId::Syncer(blockchain, network),
        started: SystemTime::now(),
        tasks: none!(),
        syncer,
        tx,
    };
    let polling = true;
    runtime.syncer.run(
        rx,
        tx_event,
        runtime.identity().into(),
        &opts,
        network,
        polling,
    )?;
    let mut service = Service::service(config, runtime)?;
    service.add_bridge_service_bus(rx_event)?;
    service.run_loop()?;
    unreachable!()
}

pub struct Runtime {
    identity: ServiceId,
    syncer: Box<dyn Synclet>,
    started: SystemTime,
    tasks: HashSet<u64>, // FIXME
    tx: Sender<SyncerdTask>,
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
        let mut notify_cli = None;
        match (&request, &source) {
            (Request::Hello, _) => {
                // Ignoring; this is used to set remote identity at ZMQ level
                info!(
                    "Service {} daemon is now {}",
                    source.bright_green_bold(),
                    "connected".bright_green_bold()
                );
            }
            (Request::SyncerTask(task), _) => {
                match self.tx.send(SyncerdTask {
                    task: task.clone(),
                    source,
                }) {
                    Ok(()) => trace!("Task successfully sent to syncer runtime"),
                    Err(e) => error!("Failed to send task with error: {}", e.to_string()),
                };
            }
            (Request::GetInfo, _) => {
                endpoints.send_to(
                    ServiceBus::Ctl,
                    self.identity(),
                    source,
                    Request::SyncerInfo(SyncerInfo {
                        uptime: SystemTime::now()
                            .duration_since(self.started)
                            .unwrap_or_else(|_| Duration::from_secs(0)),
                        since: self
                            .started
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap_or_else(|_| Duration::from_secs(0))
                            .as_secs(),
                        tasks: self.tasks.iter().cloned().collect(),
                    }),
                )?;
            }

            (Request::ListTasks, ServiceId::Client(_)) => {
                endpoints.send_to(
                    ServiceBus::Ctl,
                    self.identity(),
                    source.clone(),
                    Request::TaskList(self.tasks.iter().cloned().collect()),
                )?;
                let resp = Request::Progress(Progress::Message("ListedTasks?".to_string()));
                notify_cli = Some((Some(source), resp));
            }

            (Request::Terminate, ServiceId::Farcasterd) => {
                // terminate all runtimes
                info!("Received terminate on {}", self.identity());
                std::process::exit(0);
            }

            (req, source) => {
                error!(
                    "{} req: {}, source: {}",
                    "Request is not supported by the CTL interface".err(),
                    req,
                    source
                );
                return Err(Error::NotSupported(ServiceBus::Ctl, request.get_type()));
            }
        }

        if let Some((Some(respond_to), resp)) = notify_cli {
            endpoints.send_to(ServiceBus::Ctl, self.identity(), respond_to, resp)?;
        }

        Ok(())
    }
    fn handle_bridge(
        &mut self,
        endpoints: &mut Endpoints,
        _source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        debug!("Syncerd BRIDGE RPC request: {}", request);
        match request {
            Request::SyncerdBridgeEvent(syncerd_bridge_event) => {
                endpoints.send_to(
                    ServiceBus::Ctl,
                    self.identity(),
                    syncerd_bridge_event.source,
                    Request::SyncerEvent(syncerd_bridge_event.event),
                )?;
            }

            _ => {
                debug!("bridge request {:?} not handled here", request);
            }
        }
        Ok(())
    }
}
