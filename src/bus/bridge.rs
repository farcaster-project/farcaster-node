use crate::bus::ctl::CtlMsg;
use crate::bus::info::InfoMsg;
use crate::bus::p2p::PeerMsg;
use crate::bus::sync::SyncMsg;
use crate::bus::ServiceId;

use strict_encoding::{NetworkDecode, NetworkEncode};

#[derive(Clone, Debug, Display, From, NetworkEncode, NetworkDecode)]
#[non_exhaustive]
pub enum BridgeMsg {
    #[display("Bridge Peer {service_id}")]
    Peer {
        request: PeerMsg,
        service_id: ServiceId,
    },
    #[display("Bridge Ctl {service_id}")]
    Ctl {
        request: CtlMsg,
        service_id: ServiceId,
    },
    #[display("Bridge Rpc {service_id}")]
    Info {
        request: InfoMsg,
        service_id: ServiceId,
    },
    #[display("Bridge SyncMsg {service_id}")]
    Sync {
        request: SyncMsg,
        service_id: ServiceId,
    },
}
