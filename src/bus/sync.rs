// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use strict_encoding::{NetworkDecode, NetworkEncode};

use crate::syncerd::{Event, Task};
use crate::ServiceId;

#[derive(Clone, Debug, Display, From, NetworkEncode, NetworkDecode)]
#[non_exhaustive]
pub enum SyncMsg {
    #[display("syncer_task({0})", alt = "{0:#}")]
    #[from]
    Task(Task),

    #[display("syncer_event({0})", alt = "{0:#}")]
    #[from]
    Event(Event),

    #[display("syncer_bridge_ev({0})", alt = "{0:#}")]
    #[from]
    BridgeEvent(BridgeEvent),
}

#[derive(Clone, Debug, Display, NetworkEncode, NetworkDecode)]
#[display("{source}, {event}")]
pub struct BridgeEvent {
    pub event: Event,
    pub source: ServiceId,
}
