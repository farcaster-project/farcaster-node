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
    /// Farcaster core errors, related to low level cryptography or protocol
    #[display(inner)]
    Core(farcaster_core::Error),

    /// Generic Farcaster node errors
    #[display(inner)]
    Farcaster(String),

    /// Node configuration errors, when parsing and manipulating `farcasterd.toml` conf
    #[display("Configuration error: {0}")]
    #[from]
    Config(config::ConfigError),

    /// Generic I/O errors
    #[display(inner)]
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
    #[display("RPC request error, not supported on {0}: {1}")]
    NotSupported(ServiceBus, String),

    /// Peer does not respond to ping messages
    NotResponding,

    /// Peer has misbehaved peer protocol rules
    Misbehaving,

    /// Unrecoverable error: {0}
    Terminate(String),

    /// Other error type with string explanation
    #[display(inner)]
    #[from(internet2::addr::NoOnionSupportError)]
    Other(String),

    /// Invalid walletd token error
    InvalidToken,

    /// Syncer microservice errors
    #[display(inner)]
    #[from]
    Syncer(SyncerError),

    /// Bitcoin hashes manipulation errors
    #[display(inner)]
    #[from]
    BitcoinHashes(bitcoin::hashes::Error),

    /// Checkpoint database errors
    #[display(inner)]
    #[from]
    Checkpoint(lmdb::Error),

    /// Bitcoin key errors
    #[display(inner)]
    #[from]
    BitcoinKey(bitcoin::util::key::Error),

    /// Bitcoin secp256k1 curve errors
    #[display(inner)]
    #[from]
    BitcoinSecp256k1(bitcoin::secp256k1::Error),

    /// Bitcoin address errors
    #[display(inner)]
    #[from]
    BitcoinAddress(bitcoin::util::address::Error),

    /// Bitcoin consensus errors
    #[display(inner)]
    #[from]
    BitcoinConsensus(bitcoin::consensus::encode::Error),

    /// Bitcoin amount errors
    #[display(inner)]
    #[from]
    BitcoinAmount(bitcoin::util::amount::ParseAmountError),

    /// Monero address errors
    #[display(inner)]
    #[from]
    MoneroAddress(monero::util::address::Error),

    /// Monero amount errors
    #[display(inner)]
    #[from]
    MoneroAmount(monero::util::amount::ParsingError),

    /// Strict encoding and decoding errors
    #[display(inner)]
    #[from]
    StrictEncoding(strict_encoding::Error),

    /// Deals and swaps identifiers errors
    #[display(inner)]
    #[from]
    Uuid(uuid::Error),

    /// Internet2 address parsing errors
    #[display(inner)]
    #[from]
    Inet2AddrParseError(internet2::addr::AddrParseError),

    /// Tonic gRPC deamon transport errors
    #[display(inner)]
    #[from]
    TonicTransportError(tonic::transport::Error),
}

#[derive(Debug, Display, From, Error)]
#[display(doc_comments)]
#[non_exhaustive]
pub enum SyncerError {
    /// Generic Electrum client errors
    #[from]
    #[display(inner)]
    Electrum(electrum_client::Error),

    /// Generic Monero RPC errors
    #[from]
    #[display(inner)]
    MoneroRpc(anyhow::Error),

    /// Invalid configuration, missing or malformed
    InvalidConfig,

    /// Height did not increment
    NoIncrementToHeight,

    /// Invalid PSBT, could not construct
    InvalidPsbt,

    /// Transaction should be found in the history if we successfully queried `transaction_get`
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

//
// Custom Syncer error transformation
//

impl From<anyhow::Error> for Error {
    fn from(err: anyhow::Error) -> Self {
        Error::Syncer(SyncerError::MoneroRpc(err))
    }
}

impl From<electrum_client::Error> for Error {
    fn from(err: electrum_client::Error) -> Self {
        Error::Syncer(SyncerError::Electrum(err))
    }
}

//
// Custom Core error transformation
//

impl From<farcaster_core::Error> for Error {
    fn from(err: farcaster_core::Error) -> Self {
        Error::Core(err)
    }
}

impl From<farcaster_core::transaction::Error> for Error {
    fn from(err: farcaster_core::transaction::Error) -> Self {
        Error::Core(err.into())
    }
}

impl From<farcaster_core::crypto::Error> for Error {
    fn from(err: farcaster_core::crypto::Error) -> Self {
        Error::Core(err.into())
    }
}

impl From<farcaster_core::consensus::Error> for Error {
    fn from(err: farcaster_core::consensus::Error) -> Self {
        Error::Core(err.into())
    }
}
