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

pub mod ctl;
pub mod msg;
pub mod request;
pub mod rpc;
pub mod sync;

pub use ctl::{Failure, FailureCode};

use crate::bus::ctl::Ctl;
use crate::bus::msg::Msg;
use crate::bus::rpc::Rpc;
use crate::bus::sync::SyncMsg;
use crate::ServiceId;

use internet2::Api;
use microservices::esb::BusId;
use strict_encoding::{StrictDecode, StrictEncode};

/// Service buses used for inter-daemon communication
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Display, StrictDecode, StrictEncode)]
pub enum ServiceBus {
    /// P2P message bus
    #[display("MSG")]
    Msg,

    /// Control service bus
    #[display("CTL")]
    Ctl,

    /// RPC interface, from client to node
    #[display("RPC")]
    Rpc,

    /// Syncer interface, for syncer's tasks and events
    #[display("SYNC")]
    Sync,

    /// Bridge between listener and sender parts of a service
    #[display("BRIDGE")]
    Bridge,
}

impl BusId for ServiceBus {
    type Address = ServiceId;
}

/// Service bus messages wrapping all other message types
#[derive(Clone, Debug, Display, From, Api)]
#[api(encoding = "strict")]
#[non_exhaustive]
pub enum BusMsg {
    /// Wrapper for P2P messages to be transmitted over message bus
    #[api(type = 1)]
    #[display(inner)]
    #[from]
    Msg(Msg),

    /// Wrapper for inner type of control messages to be transmitted over the control bus
    #[api(type = 2)]
    #[display(inner)]
    #[from]
    Ctl(Ctl),

    /// Wrapper for inner type of RPC messages to be transmitted over the rpc bus
    #[api(type = 3)]
    #[display(inner)]
    #[from]
    Rpc(Rpc),

    /// Wrapper for inner type of syncer messages to be transmitted over the syncer bus
    #[api(type = 4)]
    #[display(inner)]
    #[from]
    Sync(SyncMsg),
}

impl microservices::rpc::Request for BusMsg {}
