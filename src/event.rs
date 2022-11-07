use microservices::esb;

use crate::bus::{BusMsg, ServiceBus};
use crate::Endpoints;
use crate::LogStyle;
use crate::ServiceId;

pub trait StateMachineExecutor<
    Runtime: esb::Handler<ServiceBus>,
    Error: std::error::Error,
    T: StateMachine<Runtime, Error>,
> where
    esb::Error<ServiceId>: From<<Runtime as esb::Handler<ServiceBus>>::Error>,
{
    fn execute(
        runtime: &mut Runtime,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: BusMsg,
        sm: T,
    ) -> Result<Option<T>, Error> {
        let event = Event::with(endpoints, runtime.identity(), source, request);
        let sm_display = sm.to_string();
        let sm_name = sm.name();
        if let Some(new_sm) = sm.next(event, runtime)? {
            let new_sm_display = new_sm.to_string();
            // relegate state transitions staying the same to debug
            if new_sm_display == sm_display {
                debug!(
                    "{} state self transition {}",
                    sm_name,
                    new_sm.bright_green_bold()
                );
            } else {
                info!(
                    "{} state transition {} -> {}",
                    sm_name,
                    sm_display.red_bold(),
                    new_sm.bright_green_bold()
                );
            }
            Ok(Some(new_sm))
        } else {
            info!(
                "{} state machine ended {} -> {}",
                sm_name,
                sm_display.red_bold(),
                "End".to_string().bright_green_bold()
            );
            Ok(None)
        }
    }
}

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

    fn name(&self) -> String;
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
    /// BusMsg that triggered the event
    pub request: BusMsg,
}

impl<'esb> Event<'esb> {
    /// Constructs event out of the provided data
    pub fn with(
        endpoints: &'esb mut Endpoints,
        service: ServiceId,
        source: ServiceId,
        request: BusMsg,
    ) -> Self {
        Event {
            endpoints,
            service,
            source,
            request,
        }
    }

    /// Finalizes event processing by sending reply request via CTL message bus
    pub fn complete_ctl(self, request: BusMsg) -> Result<(), esb::Error<ServiceId>> {
        self.endpoints
            .send_to(ServiceBus::Ctl, self.service, self.source, request)
    }

    /// Finalizes event processing by sending reply request via INFO message bus
    pub fn complete_info(self, request: BusMsg) -> Result<(), esb::Error<ServiceId>> {
        self.endpoints
            .send_to(ServiceBus::Info, self.service, self.source, request)
    }

    /// Finalizes event processing by sending reply request via CTL message bus to a specific
    /// service (different from the event originating service).
    pub fn complete_ctl_service(
        self,
        service: ServiceId,
        request: BusMsg,
    ) -> Result<(), esb::Error<ServiceId>> {
        self.endpoints
            .send_to(ServiceBus::Ctl, self.service, service, request)
    }

    /// Finalizes event processing by sending reply request via SYNC message bus to a specific
    /// service (different from the event originating service).
    pub fn complete_sync_service(
        self,
        service: ServiceId,
        request: BusMsg,
    ) -> Result<(), esb::Error<ServiceId>> {
        self.endpoints
            .send_to(ServiceBus::Sync, self.service, service, request)
    }

    /// Sends reply request via CTL message bus to a specific service (different from the event
    /// originating service).
    pub fn send_ctl_service(
        &mut self,
        service: ServiceId,
        request: BusMsg,
    ) -> Result<(), esb::Error<ServiceId>> {
        self.endpoints
            .send_to(ServiceBus::Ctl, self.service.clone(), service, request)
    }

    /// Sends reply request via INFO message bus to a specific service (different from the event
    /// originating service).
    pub fn send_info_service(
        &mut self,
        service: ServiceId,
        request: BusMsg,
    ) -> Result<(), esb::Error<ServiceId>> {
        self.endpoints
            .send_to(ServiceBus::Info, self.service.clone(), service, request)
    }

    /// Send reply request via MSG message bus to a specific service (different from the event originating service)
    pub fn send_msg_service(
        &mut self,
        service: ServiceId,
        request: BusMsg,
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
