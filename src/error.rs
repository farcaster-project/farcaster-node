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
use microservices::esb;

#[cfg(feature = "_rpc")]
use crate::rpc::ServiceBus;
use crate::service::ServiceId;

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

    /// Config errors: {0}
    #[from(config::ConfigError)]
    Config(config::ConfigError),

    /// I/O error: {0:?}
    #[from(io::Error)]
    Io(IoError),

    /// ESB error: {0}
    #[cfg(feature = "_rpc")]
    #[from]
    Esb(esb::Error<ServiceId>),

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
    #[from(SyncerError)]
    Syncer(SyncerError),

    /// BitcoinHashes
    #[display(inner)]
    BitcoinHashes(bitcoin::hashes::Error),

    /// Checkpoint
    #[from(lmdb::Error)]
    Checkpoint(lmdb::Error),

    /// BitcoinKey
    #[display(inner)]
    #[from(bitcoin::util::key::Error)]
    BitcoinKey(bitcoin::util::key::Error),

    /// BitcoinSecp256k1
    BitcoinSecp256k1(bitcoin::secp256k1::Error),
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

    #[display("Invalid configuration. Missing or malformed")]
    InvalidConfig,
    #[display("height did not increment")]
    NoIncrementToHeight,
    #[display("could not construct psbt")]
    InvalidPsbt,
    #[display("Transaction should be found in the history if we successfully queried `transaction_get` for it")]
    TxNotInHistory,
}

impl microservices::error::Error for Error {}

#[cfg(feature = "_rpc")]
impl From<Error> for esb::Error<ServiceId> {
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

impl From<farcaster_core::Error> for Error {
    fn from(err: farcaster_core::Error) -> Self {
        Error::FarcasterCore(err)
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

impl From<bitcoin::util::address::Error> for Error {
    fn from(err: bitcoin::util::address::Error) -> Self {
        Error::Farcaster(err.to_string())
    }
}

impl From<monero::util::amount::ParsingError> for Error {
    fn from(err: monero::util::amount::ParsingError) -> Self {
        Error::Farcaster(err.to_string())
    }
}

impl From<bitcoin::util::amount::ParseAmountError> for Error {
    fn from(err: bitcoin::util::amount::ParseAmountError) -> Self {
        Error::Farcaster(err.to_string())
    }
}

impl From<electrum_client::Error> for Error {
    fn from(err: electrum_client::Error) -> Self {
        Error::Syncer(SyncerError::Electrum(err))
    }
}

impl From<rustc_hex::FromHexError> for Error {
    fn from(err: rustc_hex::FromHexError) -> Self {
        Error::Farcaster(err.to_string())
    }
}

impl From<bitcoin::hashes::Error> for Error {
    fn from(err: bitcoin::hashes::Error) -> Self {
        Error::BitcoinHashes(err)
    }
}

impl From<bitcoin::secp256k1::Error> for Error {
    fn from(err: bitcoin::secp256k1::Error) -> Self {
        Error::BitcoinSecp256k1(err)
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
