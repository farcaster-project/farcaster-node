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

use std::thread::sleep;
use std::time::Duration;

use internet2::ZmqSocketType;
use microservices::esb;

use crate::bus::{ctl::CtlMsg, info::InfoMsg, BusMsg, ServiceBus};
use crate::service::Endpoints;
use crate::service::ServiceConfig;
use crate::{Error, LogStyle, ServiceId};

#[repr(C)]
pub struct Client {
    identity: ServiceId,
    response_queue: std::collections::VecDeque<BusMsg>,
    esb: esb::Controller<ServiceBus, BusMsg, Handler>,
}

impl Client {
    pub fn with(config: ServiceConfig) -> Result<Self, Error> {
        debug!("Setting up RPC client...");
        let identity = ServiceId::client();
        let esb = esb::Controller::with(
            map! {
                ServiceBus::Ctl => esb::BusConfig::with_addr(
                    config.ctl_endpoint,
                    ZmqSocketType::RouterConnect,
                    Some(ServiceId::router())
                ),
                ServiceBus::Info => esb::BusConfig::with_addr(
                    config.info_endpoint,
                    ZmqSocketType::RouterConnect,
                    Some(ServiceId::router()),
                )
            },
            Handler {
                identity: identity.clone(),
            },
        )?;

        // We have to sleep in order for ZMQ to bootstrap
        sleep(Duration::from_secs_f32(0.1));

        Ok(Self {
            identity,
            response_queue: empty!(),
            esb,
        })
    }

    pub fn identity(&self) -> ServiceId {
        self.identity.clone()
    }

    pub fn request(&mut self, daemon: ServiceId, req: BusMsg) -> Result<(), Error> {
        debug!("Executing {}", req);
        self.esb.send_to(ServiceBus::Info, daemon, req)?;
        Ok(())
    }

    pub fn request_info(&mut self, daemon: ServiceId, req: InfoMsg) -> Result<(), Error> {
        debug!("Executing {}", req);
        self.esb
            .send_to(ServiceBus::Info, daemon, BusMsg::Info(req))?;
        Ok(())
    }

    pub fn request_ctl(&mut self, daemon: ServiceId, req: CtlMsg) -> Result<(), Error> {
        debug!("Executing {}", req);
        self.esb
            .send_to(ServiceBus::Ctl, daemon, BusMsg::Ctl(req))?;
        Ok(())
    }

    pub fn response(&mut self) -> Result<BusMsg, Error> {
        if self.response_queue.is_empty() {
            for rep in self.esb.recv_poll()? {
                self.response_queue.push_back(rep.request);
            }
        }
        Ok(self
            .response_queue
            .pop_front()
            .expect("We always have at least one element"))
    }

    pub fn report_failure(&mut self) -> Result<BusMsg, Error> {
        match self.response()? {
            BusMsg::Ctl(CtlMsg::Failure(fail)) => Err(Error::Farcaster(fail.info)),
            resp => Ok(resp),
        }
    }

    pub fn report_response_or_fail(&mut self) -> Result<(), Error> {
        let resp = self.report_failure()?;
        // note: this triggers the yaml formatting when implemented
        println!("{}", resp);
        Ok(())
    }

    /// Print the stream of received requests until progress fails or succeed
    pub fn report_progress(&mut self) -> Result<(), Error> {
        // loop on all requests received until a progress termination condition is recieved
        // report failure transform BusMsg::Failure in error already, terminate on error or on
        // success
        loop {
            match self.report_failure() {
                Err(e) =>
                // terminate on error
                {
                    break Err(e)
                }
                Ok(BusMsg::Ctl(CtlMsg::Success(s))) => {
                    println!("{}", s.bright_green_bold());
                    // terminate on success
                    break Ok(());
                }
                Ok(req) => println!("{}", req),
            }
        }
    }
}

pub struct Handler {
    identity: ServiceId,
}

impl esb::Handler<ServiceBus> for Handler {
    type Request = BusMsg;
    type Error = Error;

    fn identity(&self) -> ServiceId {
        self.identity.clone()
    }

    fn handle(
        &mut self,
        _endpoints: &mut Endpoints,
        _bus: ServiceBus,
        _addr: ServiceId,
        _request: BusMsg,
    ) -> Result<(), Error> {
        // Cli does not receive replies for now
        Ok(())
    }

    fn handle_err(&mut self, _: &mut Endpoints, err: esb::Error<ServiceId>) -> Result<(), Error> {
        // We simply propagate the error since it's already being reported
        Err(Error::Esb(err))
    }
}
