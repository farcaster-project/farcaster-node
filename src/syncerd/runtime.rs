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
use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::ffi::OsStr;
use std::io;
use std::net::SocketAddr;
use std::process;
use std::time::{Duration, SystemTime};

use bitcoin::hashes::hex::ToHex;
use bitcoin::secp256k1;
use farcaster_core::swap::SwapId;
use internet2::{NodeAddr, RemoteSocketAddr, TypedEnum};
use lnp::{message, Messages, TempChannelId as TempSwapId};
use lnpbp::Chain;
use microservices::esb::{self, Handler};
use microservices::rpc::Failure;

use crate::rpc::request::{IntoProgressOrFalure, OptionDetails, SyncerInfo};
use crate::rpc::{request, Request, ServiceBus};
use crate::{Config, Error, LogStyle, Service, ServiceId};

pub fn run(config: Config) -> Result<(), Error> {
    let runtime = Runtime {
        identity: ServiceId::Syncer,
        chain: config.chain.clone(),
        started: SystemTime::now(),
        tasks: none!(),
        spawning_services: none!(),
    };

    Service::run(config, runtime, true)
}

pub struct Runtime {
    identity: ServiceId,
    chain: Chain,
    started: SystemTime,
    tasks: HashSet<u64>, // FIXME
    spawning_services: HashMap<ServiceId, ServiceId>,
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
            _ => Err(Error::NotSupported(ServiceBus::Bridge, request.get_type())),
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

            _ => {
                error!("MSG RPC can be only used for forwarding FWP messages");
                return Err(Error::NotSupported(ServiceBus::Msg, request.get_type()));
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
        let mut notify_cli = None;
        match (&request, &source) {
            (Request::CreateTask(task), ServiceId::Swap(swapid)) => {
                self.create_task(task, swapid)?;
            }
            (Request::Hello, _) => {
                // Ignoring; this is used to set remote identity at ZMQ level
                info!(
                    "{} daemon is {}",
                    source.bright_green_bold(),
                    "connected".bright_green_bold()
                );

            }
            (Request::GetInfo, _) => {
                senders.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Syncer,
                    source,
                    Request::SyncerInfo(SyncerInfo {
                        uptime: SystemTime::now()
                            .duration_since(self.started)
                            .unwrap_or(Duration::from_secs(0)),
                        since: self
                            .started
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap_or(Duration::from_secs(0))
                            .as_secs(),
                        tasks: self.tasks.iter().cloned().collect(),
                    }),
                )?;
            }

            (Request::ListTasks, ServiceId::Client(_)) => {
                senders.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Syncer,
                    source.clone(),
                    Request::TaskList(self.tasks.iter().cloned().collect()),
                )?;
                let resp = Request::Progress(format!("ListedTasks?",));
                notify_cli = Some((Some(source), resp));
            }

            _ => {
                error!("{}", "Request is not supported by the CTL interface".err());
                return Err(Error::NotSupported(ServiceBus::Ctl, request.get_type()));
            }
        }

        if let Some((Some(respond_to), resp)) = notify_cli {
            senders.send_to(ServiceBus::Ctl, ServiceId::Syncer, respond_to, resp)?;
        }

        Ok(())
    }
    // TODO
    fn create_task(&mut self, task: &u64, swapid: &SwapId) -> Result<(), Error> {
        Ok(())
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
