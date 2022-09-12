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

pub mod client;
pub mod ctl;
pub mod msg;
pub mod request;
pub mod rpc;
pub mod sync;

use crate::ServiceId;
pub use client::Client;
#[cfg(feature = "shell")]
pub use request::Request;
pub use ctl::{Failure, FailureCode};

use microservices::esb::BusId;
use strict_encoding::{StrictDecode, StrictEncode};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Display, StrictEncode, StrictDecode)]
pub enum ServiceBus {
    #[display("MSG")]
    Msg,
    #[display("CTL")]
    Ctl,
    #[display("RPC")]
    Rpc,
    #[display("SYNC")]
    Sync,
    #[display("BRIDGE")]
    Bridge,
}

impl BusId for ServiceBus {
    type Address = ServiceId;
}

pub struct Rpc {}
