use internet2::Api;
use strict_encoding::{StrictDecode, StrictEncode};

use crate::syncerd::{Event, Task};
use crate::ServiceId;

#[derive(Clone, Debug, Display, From, StrictDecode, StrictEncode, Api)]
#[api(encoding = "strict")]
#[non_exhaustive]
pub enum SyncMsg {
    #[api(type = 1300)]
    #[display("syncer_task({0})", alt = "{0:#}")]
    #[from]
    Task(Task),

    #[api(type = 1301)]
    #[display("syncer_event({0})", alt = "{0:#}")]
    #[from]
    Event(Event),

    #[api(type = 1302)]
    #[display("syncer_bridge_ev({0})", alt = "{0:#}")]
    #[from]
    BridgeEvent(BridgeEvent),
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("{source}, {event}")]
pub struct BridgeEvent {
    pub event: Event,
    pub source: ServiceId,
}
