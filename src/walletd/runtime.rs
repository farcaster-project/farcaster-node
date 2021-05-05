use crate::rpc::{Request, ServiceBus};
use crate::Senders;
use crate::{Config, CtlServer, Error, Service, ServiceId};
use bitcoin::secp256k1;
use internet2::{LocalNode, TypedEnum};
use microservices::esb::{self, Handler};

pub fn run(config: Config) -> Result<(), Error> {
    let runtime = Runtime {
        identity: ServiceId::Wallet,
    };

    Service::run(config, runtime, false)
}

pub struct Runtime {
    identity: ServiceId,
}

impl CtlServer for Runtime {}

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
            _ => {
                Err(Error::NotSupported(ServiceBus::Bridge, request.get_type()))
            }
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
        _senders: &mut Senders,
        _source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        match request {
            Request::Hello => {
                // Ignoring; this is used to set remote identity at ZMQ level
            }
            _ => {
                error!("MSG RPC can only be used for farwarding LNPBP messages")
            }
        }
        Ok(())
    }

    fn handle_rpc_ctl(
        &mut self,
        _senders: &mut Senders,
        _source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        match request {
            _ => {
                error!("Request is not supported by the CTL interface");
                // return Err(Error::NotSupported(
                //     ServiceBus::Ctl,
                //     request.get_type(),
                // ));
            }
        }
        Ok(())
    }
}
