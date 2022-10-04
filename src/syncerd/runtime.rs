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

use crate::bus::{
    ctl::Ctl,
    rpc::{Rpc, SyncerInfo},
    sync::SyncMsg,
    BusMsg, ServiceBus,
};
use crate::service::Endpoints;
use crate::syncerd::bitcoin_syncer::BitcoinSyncer;
use crate::syncerd::monero_syncer::MoneroSyncer;
use crate::syncerd::opts::Opts;
use crate::syncerd::*;
use crate::CtlServer;
use crate::{Error, LogStyle, Service, ServiceConfig, ServiceId};

use std::collections::HashSet;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::time::{Duration, SystemTime};

use farcaster_core::blockchain::{Blockchain, Network};
use microservices::esb::{self, Handler};
use microservices::ZMQ_CONTEXT;
use strict_encoding::{StrictDecode, StrictEncode};

pub trait Synclet {
    fn run(
        &mut self,
        rx: Receiver<SyncerdTask>,
        tx: zmq::Socket,
        syncer_address: Vec<u8>,
        opts: &Opts,
        network: Network,
    ) -> Result<(), Error>;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Display, StrictEncode, StrictDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
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
    runtime
        .syncer
        .run(rx, tx_event, runtime.identity().into(), &opts, network)?;
    let mut service = Service::service(config, runtime)?;
    service.add_bridge_service_bus(rx_event)?;
    service.run_loop()?;
    unreachable!()
}

pub struct Runtime {
    identity: ServiceId,
    syncer: Box<dyn Synclet>,
    started: SystemTime,
    tasks: HashSet<SyncerdTask>,
    tx: Sender<SyncerdTask>,
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
            // FIXME: to be removed
            (ServiceBus::Msg, request) => self.handle_msg(endpoints, source, request),
            // Control bus for issuing control commands, only accept BusMsg::Ctl
            (ServiceBus::Ctl, BusMsg::Ctl(req)) => self.handle_ctl(endpoints, source, req),
            // RPC command bus, only accept BusMsg::Rpc
            (ServiceBus::Rpc, BusMsg::Rpc(req)) => self.handle_rpc(endpoints, source, req),
            // Syncer event bus for blockchain tasks and events, only accept BusMsg::Sync
            (ServiceBus::Sync, BusMsg::Sync(req)) => self.handle_sync(endpoints, source, req),
            // Internal syncer bridge for inner communication, only accept BusMsg::Sync
            (ServiceBus::Bridge, BusMsg::Sync(req)) => self.handle_bridge(endpoints, source, req),
            // All other pairs are not supported
            (_, request) => Err(Error::NotSupported(bus, request.to_string())),
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
    fn handle_msg(
        &mut self,
        _endpoints: &mut Endpoints,
        _source: ServiceId,
        request: BusMsg,
    ) -> Result<(), Error> {
        match request {
            BusMsg::Ctl(Ctl::Hello) => {
                // Ignoring; this is used to set remote identity at ZMQ level
            }

            _ => {
                error!("MSG RPC can be only used for forwarding farcaster protocol messages");
                return Err(Error::NotSupported(ServiceBus::Msg, request.to_string()));
            }
        }
        Ok(())
    }

    fn handle_ctl(
        &mut self,
        _endpoints: &mut Endpoints,
        source: ServiceId,
        request: Ctl,
    ) -> Result<(), Error> {
        match (&request, &source) {
            (Ctl::Hello, _) => {
                // Ignoring; this is used to set remote identity at ZMQ level
                info!(
                    "Service {} daemon is now {}",
                    source.bright_green_bold(),
                    "connected".bright_green_bold()
                );
            }

            (Ctl::Terminate, ServiceId::Farcasterd) => {
                // terminate all runtimes
                info!("Received terminate on {}", self.identity());
                std::process::exit(0);
            }

            (req, source) => {
                error!(
                    "{} req: {}, source: {}",
                    "BusMsg is not supported by the CTL interface".err(),
                    req,
                    source
                );
                return Err(Error::NotSupported(ServiceBus::Ctl, request.to_string()));
            }
        }

        Ok(())
    }

    fn handle_rpc(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: Rpc,
    ) -> Result<(), Error> {
        match request {
            Rpc::GetInfo => {
                self.send_client_rpc(
                    endpoints,
                    source,
                    Rpc::SyncerInfo(SyncerInfo {
                        syncer: self.identity().to_string(),
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

            Rpc::ListTasks => {
                self.send_client_rpc(
                    endpoints,
                    source,
                    Rpc::TaskList(self.tasks.iter().cloned().collect()),
                )?;
            }

            req => {
                warn!("Ignoring request: {}", req.err());
            }
        }

        Ok(())
    }

    fn handle_sync(
        &mut self,
        _endpoints: &mut Endpoints,
        source: ServiceId,
        request: SyncMsg,
    ) -> Result<(), Error> {
        match request {
            SyncMsg::Task(task) => {
                let t = SyncerdTask {
                    task: task.clone(),
                    source,
                };
                self.tasks.insert(t.clone());
                match self.tx.send(t) {
                    Ok(()) => trace!("Task successfully sent to syncer runtime"),
                    Err(e) => error!("Failed to send task with error: {}", e.to_string()),
                };
            }

            req => {
                warn!("Ignoring request: {}", req.err());
            }
        }

        Ok(())
    }

    fn handle_bridge(
        &mut self,
        endpoints: &mut Endpoints,
        _source: ServiceId,
        request: SyncMsg,
    ) -> Result<(), Error> {
        debug!("Syncerd BRIDGE RPC request: {}", request);
        match request {
            SyncMsg::BridgeEvent(syncerd_bridge_event) => {
                endpoints.send_to(
                    ServiceBus::Sync,
                    self.identity(),
                    syncerd_bridge_event.source,
                    BusMsg::Sync(SyncMsg::Event(syncerd_bridge_event.event)),
                )?;
            }

            _ => {
                debug!("bridge request {:?} not handled here", request);
            }
        }
        Ok(())
    }
}
