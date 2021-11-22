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

use std::io;

use amplify::IoError;
#[cfg(feature = "_rpc")]
use internet2::TypeId;
use internet2::{presentation, transport};
#[cfg(feature = "_rpc")]
use microservices::{esb, rpc};

#[cfg(feature = "_rpc")]
use crate::rpc::ServiceBus;

#[derive(Debug, Display, From, Error)]
#[display(doc_comments)]
#[non_exhaustive]
pub enum Error {
    /// Farcaster core errors: {0}
    FarcasterCore(farcaster_core::Error),

    /// Farcaster node errors: {0}
    Farcaster(String),

    /// Wallet node errors: {0}
    Wallet(String),
    /// I/O error: {0:?}
    #[from(io::Error)]
    Io(IoError),

    /// ESB error: {0}
    #[cfg(feature = "_rpc")]
    #[from]
    Esb(esb::Error),

    /// RPC error: {0}
    #[cfg(feature = "_rpc")]
    #[from]
    Rpc(rpc::Error),

    /// Peer interface error: {0}
    #[from]
    Peer(presentation::Error),

    /// Bridge interface error: {0}
    #[cfg(any(feature = "node", feature = "client"))]
    #[from(zmq::Error)]
    #[from]
    Bridge(transport::Error),

    /// Provided RPC request is not supported for the used type of endpoint
    #[cfg(feature = "_rpc")]
    NotSupported(ServiceBus, TypeId),

    /// Peer does not respond to ping messages
    NotResponding,

    /// Peer has misbehaved LN peer protocol rules
    Misbehaving,

    /// unrecoverable error "{0}"
    Terminate(String),

    /// Other error type with string explanation
    #[display(inner)]
    #[from(internet2::addr::NoOnionSupportError)]
    Other(String),

    /// Invalid walletd token error
    InvalidToken,

    /// Syncer
    #[display(inner)]
    Syncer(SyncerError),

    /// BitcoinHashes
    #[display(inner)]
    BitcoinHashes(bitcoin::hashes::Error),
}

#[derive(Debug, Display)]
pub enum SyncerError {
    #[display(inner)]
    Electrum(electrum_client::Error),
    #[display(inner)]
    NoTxsOnAddress,
    #[display(inner)]
    ScriptAlreadyRegistered,
    #[display("syncer creating error")]
    UnknownNetwork,
    #[display(inner)]
    MoneroRpc(anyhow::Error),
    #[display(inner)]
    InvalidConfig,
    #[display("height did not increment")]
    NoIncrementToHeight,
}

impl microservices::error::Error for Error {}

#[cfg(feature = "_rpc")]
impl From<Error> for esb::Error {
    fn from(err: Error) -> Self {
        match err {
            Error::Esb(err) => err,
            err => esb::Error::ServiceError(err.to_string()),
        }
    }
}

impl From<anyhow::Error> for Error {
    fn from(err: anyhow::Error) -> Self {
        Error::Syncer(SyncerError::MoneroRpc(err))
    }
}

#[cfg(feature = "_rpc")]
impl From<Error> for rpc::Error {
    fn from(err: Error) -> Self {
        match err {
            Error::Rpc(err) => err,
            err => rpc::Error::ServerFailure(rpc::Failure {
                code: 2000,
                info: err.to_string(),
            }),
        }
    }
}

impl From<farcaster_core::Error> for Error {
    fn from(err: farcaster_core::Error) -> Self {
        Error::FarcasterCore(err)
    }
}

impl From<farcaster_core::syncer::Error> for Error {
    fn from(err: farcaster_core::syncer::Error) -> Self {
        Error::Farcaster(err.to_string())
    }
}

impl From<bitcoin::consensus::encode::Error> for Error {
    fn from(err: bitcoin::consensus::encode::Error) -> Self {
        Error::Farcaster(err.to_string())
    }
}

impl From<monero::util::address::Error> for Error {
    fn from(err: monero::util::address::Error) -> Self {
        Error::Farcaster(err.to_string())
    }
}

impl From<electrum_client::Error> for Error {
    fn from(err: electrum_client::Error) -> Self {
        Error::Syncer(SyncerError::Electrum(err))
    }
}

impl From<bitcoin::hashes::Error> for Error {
    fn from(err: bitcoin::hashes::Error) -> Self {
        Error::BitcoinHashes(err)
    }
}

impl From<farcaster_core::transaction::Error> for Error {
    fn from(err: farcaster_core::transaction::Error) -> Self {
        Error::FarcasterCore(err.into())
    }
}

impl From<farcaster_core::crypto::Error> for Error {
    fn from(err: farcaster_core::crypto::Error) -> Self {
        Error::FarcasterCore(err.into())
    }
}

impl From<farcaster_core::consensus::Error> for Error {
    fn from(err: farcaster_core::consensus::Error) -> Self {
        Error::FarcasterCore(err.into())
    }
}
