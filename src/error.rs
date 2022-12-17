// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use std::io;

use amplify::IoError;
use internet2::{presentation, transport};
#[cfg(feature = "_rpc")]
use microservices::esb;

#[cfg(feature = "_rpc")]
use crate::bus::ServiceBus;
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
    NotSupported(ServiceBus, String),

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
    #[from]
    Syncer(SyncerError),

    /// BitcoinHashes
    #[display(inner)]
    #[from]
    BitcoinHashes(bitcoin::hashes::Error),

    /// LMDB
    #[display(inner)]
    #[from]
    Checkpoint(lmdb::Error),

    /// BitcoinKey
    #[display(inner)]
    #[from]
    BitcoinKey(bitcoin::util::key::Error),

    /// BitcoinSecp256k1
    #[display(inner)]
    #[from]
    BitcoinSecp256k1(bitcoin::secp256k1::Error),

    #[display(inner)]
    #[from]
    BitcoinAddress(bitcoin::util::address::Error),

    #[display(inner)]
    #[from]
    BitcoinConsensus(bitcoin::consensus::encode::Error),

    #[display(inner)]
    #[from]
    BitcoinAmount(bitcoin::util::amount::ParseAmountError),

    #[display(inner)]
    #[from]
    MoneroAddress(monero::util::address::Error),

    #[display(inner)]
    #[from]
    MoneroAmount(monero::util::amount::ParsingError),

    #[display(inner)]
    #[from]
    RustHex(rustc_hex::FromHexError),

    /// StrictEncoding
    #[display(inner)]
    #[from]
    StrictEncoding(strict_encoding::Error),

    /// Uuid
    #[display(inner)]
    #[from]
    Uuid(uuid::Error),

    /// Inet2AddrParseError
    #[display(inner)]
    #[from]
    Inet2AddrParseError(internet2::addr::AddrParseError),

    /// TonicTransportError
    #[display(inner)]
    #[from]
    TonicTransportError(tonic::transport::Error),
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

impl From<electrum_client::Error> for Error {
    fn from(err: electrum_client::Error) -> Self {
        Error::Syncer(SyncerError::Electrum(err))
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
