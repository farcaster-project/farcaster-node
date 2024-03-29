// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

pub mod bridge;
pub mod ctl;
pub mod info;
pub mod p2p;
pub mod sync;
mod types;

// Import all shared types
pub use types::*;

use std::fmt::{self, Debug, Display, Formatter};
use std::iter::FromIterator;

use crate::bus::bridge::BridgeMsg;
use crate::bus::ctl::CtlMsg;
use crate::bus::info::InfoMsg;
use crate::bus::p2p::PeerMsg;
use crate::bus::sync::SyncMsg;
use crate::ServiceId;

use amplify::Wrapper;
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

    /// Info interface, from client to node to read data
    #[display("INFO")]
    Info,

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
    P2p(PeerMsg),

    /// Wrapper for inner type of control messages to be transmitted over the control bus
    #[api(type = 2)]
    #[display(inner)]
    #[from]
    Ctl(CtlMsg),

    /// Wrapper for inner type of info messages to be transmitted over the info bus
    #[api(type = 3)]
    #[display(inner)]
    #[from]
    Info(InfoMsg),

    /// Wrapper for inner type of syncer messages to be transmitted over the syncer bus
    #[api(type = 4)]
    #[display(inner)]
    #[from]
    Sync(SyncMsg),

    /// Wrapper for inner type of bridge messages to be transmitted over bridge busses
    #[api(type = 5)]
    #[display(inner)]
    #[from]
    Bridge(BridgeMsg),
}

impl microservices::rpc::Request for BusMsg {}

impl From<crate::Error> for BusMsg {
    fn from(err: crate::Error) -> Self {
        BusMsg::Ctl(CtlMsg::Failure(Failure {
            code: FailureCode::Unknown,
            info: err.to_string(),
        }))
    }
}

/// An encodable list that is serializable in yaml
#[derive(Wrapper, Clone, PartialEq, Eq, Debug, From, StrictEncode, StrictDecode)]
#[wrapper(IndexRange)]
pub struct List<T>(Vec<T>)
where
    T: Clone + PartialEq + Eq + Debug + Display + StrictEncode + StrictDecode;

#[cfg(feature = "serde")]
impl<T> Display for List<T>
where
    T: Clone + PartialEq + Eq + Debug + Display + serde::Serialize + StrictEncode + StrictDecode,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&serde_yaml::to_string(self).expect("internal YAML serialization error"))
    }
}

impl<T> FromIterator<T> for List<T>
where
    T: Clone + PartialEq + Eq + Debug + Display + serde::Serialize + StrictEncode + StrictDecode,
{
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::from_inner(iter.into_iter().collect())
    }
}

#[cfg(feature = "serde")]
impl<T> serde::Serialize for List<T>
where
    T: Clone + PartialEq + Eq + Debug + Display + serde::Serialize + StrictEncode + StrictDecode,
{
    fn serialize<S>(
        &self,
        serializer: S,
    ) -> Result<<S as serde::Serializer>::Ok, <S as serde::Serializer>::Error>
    where
        S: serde::Serializer,
    {
        self.as_inner().serialize(serializer)
    }
}
