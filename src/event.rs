use microservices::esb;

use crate::rpc::Request;
use crate::rpc::ServiceBus;
use crate::Endpoints;
use crate::ServiceId;

/// State machine used by runtimes for managing complex asynchronous workflows
pub trait StateMachine<Runtime: esb::Handler<ServiceBus>, Error: std::error::Error>:
    std::fmt::Display
where
    esb::Error<ServiceId>: From<<Runtime as esb::Handler<ServiceBus>>::Error>,
{
    /// Move state machine to a next step in response to the provided event.
    /// At the completion of the cycle the state machine is consumed and `Ok(None)` is returned.
    fn next(self, event: Event, runtime: &mut Runtime) -> Result<Option<Self>, Error>
    where
        Self: Sized;
}

/// Event changing state machine state, consisting of a certain P2P or RPC `request` sent from some
/// service `source` to the current `service`.
pub struct Event<'esb> {
    /// ESB API provided by a controller
    pub endpoints: &'esb mut Endpoints,
    /// Local service id (event receiver)
    pub service: ServiceId,
    /// Remote service id (event originator)
    pub source: ServiceId,
    /// Request that triggered the event
    pub request: Request,
}

impl<'esb> Event<'esb> {
    /// Constructs event out of the provided data
    pub fn with(
        endpoints: &'esb mut Endpoints,
        service: ServiceId,
        source: ServiceId,
        request: Request,
    ) -> Self {
        Event {
            endpoints,
            service,
            source,
            request,
        }
    }

    /// Finalizes event processing by sending reply request via CTL message bus
    pub fn complete_ctl(self, request: Request) -> Result<(), esb::Error<ServiceId>> {
        self.endpoints
            .send_to(ServiceBus::Ctl, self.service, self.source, request)
    }

    /// Finalizes event processing by sending reply request via CTL message bus to a specific
    /// service (different from the event originating service).
    pub fn complete_ctl_service(
        self,
        service: ServiceId,
        request: Request,
    ) -> Result<(), esb::Error<ServiceId>> {
        self.endpoints
            .send_to(ServiceBus::Ctl, self.service, service, request)
    }

    /// Sends reply request via CTL message bus to a specific service (different from the event
    /// originating service).
    pub fn send_ctl_service(
        &mut self,
        service: ServiceId,
        request: Request,
    ) -> Result<(), esb::Error<ServiceId>> {
        self.endpoints
            .send_to(ServiceBus::Ctl, self.service.clone(), service, request)
    }

    /// Send reply request via MSG message bus to a specific service (different from the event originating service)
    pub fn send_msg_service(
        &mut self,
        service: ServiceId,
        request: Request,
    ) -> Result<(), esb::Error<ServiceId>> {
        self.endpoints
            .send_to(ServiceBus::Msg, self.service.clone(), service, request)
    }

    /// Forwards a request through bus to service (new source is the service forwarding)
    pub fn forward_msg(&mut self, service: ServiceId) -> Result<(), esb::Error<ServiceId>> {
        self.endpoints.send_to(
            ServiceBus::Msg,
            self.service.clone(),
            service,
            self.request.clone(),
        )
    }
}
