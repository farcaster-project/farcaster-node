use monero::consensus::Decodable;
use monero::consensus::Encodable;
use std::io;
use strict_encoding::{StrictDecode, StrictEncode};

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub enum AddressAddendum {
    Monero(XmrAddressAddendum),
    Bitcoin(BtcAddressAddendum),
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct BtcAddressAddendum {
    /// The address the syncer will watch and query.
    pub address: Option<bitcoin::Address>,
    /// The blockchain height where to start the query (not inclusive).
    pub from_height: u64,
    /// The associated script pubkey used by server like Electrum.
    pub script_pubkey: bitcoin::Script,
}

#[derive(Clone, Debug, Display, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct XmrAddressAddendum {
    pub spend_key: monero::PublicKey,
    pub view_key: monero::PrivateKey,
    /// The blockchain height where to start the query (not inclusive).
    pub from_height: u64,
}

impl StrictEncode for XmrAddressAddendum {
    fn strict_encode<E: ::std::io::Write>(
        &self,
        mut e: E,
    ) -> Result<usize, strict_encoding::Error> {
        let mut len = self
            .spend_key
            .consensus_encode(&mut e)
            .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?;
        len += self
            .view_key
            .consensus_encode(&mut e)
            .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?;
        Ok(len
            + self
                .from_height
                .consensus_encode(&mut e)
                .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?)
    }
}

impl StrictDecode for XmrAddressAddendum {
    fn strict_decode<D: ::std::io::Read>(mut d: D) -> Result<Self, strict_encoding::Error> {
        Ok(Self {
            spend_key: monero::PublicKey::consensus_decode(&mut d)
                .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?,
            view_key: monero::PrivateKey::consensus_decode(&mut d)
                .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?,
            from_height: u64::consensus_decode(&mut d)
                .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?,
        })
    }
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct SweepAddress {
    pub id: u32,
    pub lifetime: u64,
    pub addendum: SweepAddressAddendum,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub enum SweepAddressAddendum {
    Monero(SweepXmrAddress),
    Bitcoin(SweepBitcoinAddress),
}

#[derive(Clone, Debug, Display, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct SweepXmrAddress {
    pub spend_key: monero::PrivateKey,
    pub view_key: monero::PrivateKey,
    pub address: monero::Address,
}

impl StrictEncode for SweepXmrAddress {
    fn strict_encode<E: ::std::io::Write>(
        &self,
        mut e: E,
    ) -> Result<usize, strict_encoding::Error> {
        let mut len = self
            .spend_key
            .consensus_encode(&mut e)
            .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?;
        len += self
            .view_key
            .consensus_encode(&mut e)
            .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?;
        Ok(len
            + self
                .address
                .consensus_encode(&mut e)
                .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?)
    }
}

impl StrictDecode for SweepXmrAddress {
    fn strict_decode<D: ::std::io::Read>(mut d: D) -> Result<Self, strict_encoding::Error> {
        Ok(Self {
            spend_key: monero::PrivateKey::consensus_decode(&mut d)
                .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?,
            view_key: monero::PrivateKey::consensus_decode(&mut d)
                .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?,
            address: monero::Address::consensus_decode(&mut d)
                .map_err(|e| strict_encoding::Error::DataIntegrityError(e.to_string()))?,
        })
    }
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct SweepBitcoinAddress {
    pub private_key: [u8; 32],
    pub address: bitcoin::Address,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct Abort {
    pub task_target: TaskTarget,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
pub enum TaskTarget {
    TaskId(u32),
    AllTasks,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct WatchHeight {
    pub id: u32,
    pub lifetime: u64,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct WatchAddress {
    pub id: u32,
    pub lifetime: u64,
    pub addendum: AddressAddendum,
    pub include_tx: Boolean,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub enum Boolean {
    True,
    False,
}

impl From<Boolean> for bool {
    fn from(w: Boolean) -> bool {
        match w {
            Boolean::True => true,
            Boolean::False => false,
        }
    }
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct WatchTransaction {
    pub id: u32,
    pub lifetime: u64,
    pub hash: Vec<u8>,
    pub confirmation_bound: u32,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct BroadcastTransaction {
    pub id: u32,
    pub tx: Vec<u8>,
}

/// Tasks created by the daemon and handle by syncers to process a blockchain
/// and generate [`Event`] back to the syncer.
#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub enum Task {
    Abort(Abort),
    WatchHeight(WatchHeight),
    WatchAddress(WatchAddress),
    WatchTransaction(WatchTransaction),
    BroadcastTransaction(BroadcastTransaction),
    SweepAddress(SweepAddress),
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct TaskAborted {
    pub id: Vec<u32>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct HeightChanged {
    pub id: u32,
    pub block: Vec<u8>,
    pub height: u64,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct AddressTransaction {
    pub id: u32,
    pub hash: Vec<u8>,
    pub amount: u64,
    pub block: Vec<u8>,
    pub tx: Vec<u8>,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct TransactionConfirmations {
    pub id: u32,
    pub block: Vec<u8>,
    pub confirmations: Option<u32>,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct TransactionBroadcasted {
    pub id: u32,
    pub tx: Vec<u8>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct SweepSuccess {
    pub id: u32,
    pub txids: Vec<Vec<u8>>,
}

/// Events returned by syncers to the daemon to update the blockchain states.
/// Events are identified with a unique 32-bits integer that match the [`Task`]
/// id.
#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub enum Event {
    /// Notify the daemon the blockchain height changed.
    HeightChanged(HeightChanged),
    AddressTransaction(AddressTransaction),
    TransactionConfirmations(TransactionConfirmations),
    TransactionBroadcasted(TransactionBroadcasted),
    SweepSuccess(SweepSuccess),
    /// Notify the daemon the task has been aborted with success or failure.
    /// Carries the status for the task abortion.
    TaskAborted(TaskAborted),
}
