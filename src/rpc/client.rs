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
use std::convert::TryInto;
use std::thread::{self, sleep};
use std::time::Duration;

use internet2::ZmqType;
use microservices::esb;

use crate::rpc::request::OptionDetails;
use crate::rpc::request::SwapProgress;
use crate::rpc::{Request, ServiceBus};
use crate::service::ServiceConfig;
use crate::{Error, LogStyle, ServiceId};

#[repr(C)]
pub struct Client {
    identity: ServiceId,
    response_queue: std::collections::VecDeque<Request>,
    esb: esb::Controller<ServiceBus, Request, Handler>,
}

impl Client {
    pub fn with(config: ServiceConfig) -> Result<Self, Error> {
        debug!("Setting up RPC client...");
        let identity = ServiceId::client();
        let bus_config = esb::BusConfig::with_locator(
            config
                .ctl_endpoint
                .try_into()
                .expect("Only ZMQ RPC is currently supported"),
            Some(ServiceId::router()),
        );
        let esb = esb::Controller::with(
            map! {
                ServiceBus::Ctl => bus_config
            },
            Handler {
                identity: identity.clone(),
            },
            ZmqType::RouterConnect,
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

    pub fn request(&mut self, daemon: ServiceId, req: Request) -> Result<(), Error> {
        debug!("Executing {}", req);
        self.esb.send_to(ServiceBus::Ctl, daemon, req)?;
        Ok(())
    }

    pub fn response(&mut self) -> Result<Request, Error> {
        if self.response_queue.is_empty() {
            for (_, _, rep) in self.esb.recv_poll()? {
                self.response_queue.push_back(rep);
            }
        }
        Ok(self
            .response_queue
            .pop_front()
            .expect("We always have at least one element"))
    }

    pub fn report_failure(&mut self) -> Result<Request, Error> {
        match self.response()? {
            Request::Failure(fail) => Err(Error::from(fail)),
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
        // report failure transform Request::Failure in error already, terminate on error or on
        // success
        let res = loop {
            match self.report_failure() {
                Err(e) => {
                    // terminate on error
                    break Err(e);
                }
                Ok(Request::Success(s)) => {
                    println!("{}", s.bright_green_bold());
                    // terminate on success
                    break Ok(());
                }
                Ok(req) => println!("{}", req),
            }
        };
        res
    }
}

pub struct Handler {
    identity: ServiceId,
}

impl esb::Handler<ServiceBus> for Handler {
    type Request = Request;
    type Error = Error;

    fn identity(&self) -> ServiceId {
        self.identity.clone()
    }

    fn handle(
        &mut self,
        _endpoints: &mut Endpoints,
        _bus: ServiceBus,
        _addr: ServiceId,
        _request: Request,
    ) -> Result<(), Error> {
        // Cli does not receive replies for now
        Ok(())
    }

    fn handle_err(&mut self, _: &mut Endpoints, err: esb::Error<ServiceId>) -> Result<(), Error> {
        // We simply propagate the error since it's already being reported
        Err(Error::Esb(err))
    }
}
