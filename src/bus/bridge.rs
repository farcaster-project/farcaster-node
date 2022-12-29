// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

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
    #[display("Grpc Server Terminated")]
    GrpcServerTerminated,
}
