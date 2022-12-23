// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use std::fmt;

use bitcoin::consensus::Decodable;
#[cfg(feature = "serde")]
use serde_with::DisplayFromStr;
use strict_encoding::{StrictDecode, StrictEncode};

use crate::bus::{info::Address, AddressSecretKey};

// The strict encoding length limit
pub const STRICT_ENCODE_MAX_ITEMS: u16 = u16::MAX - 1;

#[derive(
    Clone, Copy, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Ord, PartialOrd, Hash,
)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub struct TaskId(pub u32);

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub enum AddressAddendum {
    #[display("Monero Address {0}")]
    Monero(XmrAddressAddendum),
    #[display("Bitcoin Address {0}")]
    Bitcoin(BtcAddressAddendum),
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub struct BtcAddressAddendum {
    /// The blockchain height where to start the query (not inclusive).
    pub from_height: u64,
    /// The address to be watched.
    pub address: bitcoin::Address,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, Debug, Display, Eq, PartialEq, Hash, StrictEncode, StrictDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display("address: {address}, from_height: {from_height}")]
pub struct XmrAddressAddendum {
    pub address: monero::Address,
    #[serde_as(as = "DisplayFromStr")]
    pub view_key: monero::PrivateKey,
    /// The blockchain height where to start the query (not inclusive).
    pub from_height: u64,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display("SweepAddress({addendum}, retry: {retry}, id: {id}, lifetime: {lifetime})")]
pub struct SweepAddress {
    pub retry: bool,
    pub id: TaskId,
    pub lifetime: u64,
    pub addendum: SweepAddressAddendum,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub enum SweepAddressAddendum {
    #[display("SweepMoneroAddress({0})")]
    Monero(SweepMoneroAddress),
    #[display("SweepBitcoinAddress({0})")]
    Bitcoin(SweepBitcoinAddress),
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, Debug, Display, Eq, PartialEq, Hash, StrictEncode, StrictDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display("Sweep destination: {destination_address}, Min balance: {minimum_balance}")]
pub struct SweepMoneroAddress {
    #[serde_as(as = "DisplayFromStr")]
    pub source_spend_key: monero::PrivateKey,
    #[serde_as(as = "DisplayFromStr")]
    pub source_view_key: monero::PrivateKey,
    pub destination_address: monero::Address,
    #[serde(with = "monero::util::amount::serde::as_xmr")]
    pub minimum_balance: monero::Amount,
    pub from_height: Option<u64>,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display("Source address: {source_address}, destination_address: {destination_address}")]
pub struct SweepBitcoinAddress {
    pub source_secret_key: bitcoin::secp256k1::SecretKey,
    pub source_address: bitcoin::Address,
    pub destination_address: bitcoin::Address,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub struct Abort {
    pub task_target: TaskTarget,
    pub respond: Boolean,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub enum TaskTarget {
    TaskId(TaskId),
    AllTasks,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub struct WatchHeight {
    pub id: TaskId,
    pub lifetime: u64,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display("WatchAddress({addendum}, id: {id}, lifetime: {lifetime}, include_tx: {include_tx})")]
pub struct WatchAddress {
    pub id: TaskId,
    pub lifetime: u64,
    pub addendum: AddressAddendum,
    pub include_tx: Boolean,
    pub filter: TxFilter,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub enum TxFilter {
    Incoming,
    Outgoing,
    All,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
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

#[derive(Clone, Display, Debug, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display("WatchTransaction(id: {id}, lifetime: {lifetime}, hash: {hash}, confirmation_bound: {confirmation_bound})")]
pub struct WatchTransaction {
    pub id: TaskId,
    pub lifetime: u64,
    pub hash: Txid,
    pub confirmation_bound: u32,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub struct BroadcastTransaction {
    pub id: TaskId,
    #[serde(with = "hex")]
    pub tx: Vec<u8>,
    pub broadcast_after_height: Option<u64>,
}

impl fmt::Display for BroadcastTransaction {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let bitcoin_tx_id =
            bitcoin::Transaction::consensus_decode(std::io::Cursor::new(self.tx.clone()))
                .map(|tx| tx.txid().to_string())
                .unwrap_or_default();

        write!(
            f,
            "BroadcastTransaction(id: {}, broadcast_after_height: {:?}, tx_id: {})",
            self.id, self.broadcast_after_height, bitcoin_tx_id
        )
    }
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display("GetTx(id: {id}, hash: {hash})")]
pub struct GetTx {
    pub id: TaskId,
    pub hash: Txid,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub struct WatchEstimateFee {
    pub id: TaskId,
    pub lifetime: u64,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub struct HealthCheck {
    pub id: TaskId,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub struct GetAddressBalance {
    pub id: TaskId,
    pub address_secret_key: AddressSecretKey,
}

/// Tasks created by the daemon and handle by syncers to process a blockchain
/// and generate [`Event`] back to the syncer.
#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub enum Task {
    #[display("{0}")]
    Abort(Abort),
    #[display("{0}")]
    WatchHeight(WatchHeight),
    #[display("{0}")]
    WatchAddress(WatchAddress),
    #[display("{0}")]
    WatchTransaction(WatchTransaction),
    #[display("{0}")]
    BroadcastTransaction(BroadcastTransaction),
    #[display("{0}")]
    SweepAddress(SweepAddress),
    #[display("{0}")]
    GetTx(GetTx),
    #[display("{0}")]
    GetAddressBalance(GetAddressBalance),
    #[display("{0}")]
    WatchEstimateFee(WatchEstimateFee),
    #[display("{0}")]
    HealthCheck(HealthCheck),
    #[display("Terminate")]
    Terminate,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct TaskAborted {
    pub id: Vec<TaskId>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
pub struct HeightChanged {
    pub id: TaskId,
    pub block: Vec<u8>,
    pub height: u64,
}

impl fmt::Display for HeightChanged {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "HeightChanged(id: {}, block: {}, height: {})",
            self.id,
            hex::encode(&self.block),
            self.height,
        )
    }
}

#[derive(Copy, Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub enum Txid {
    #[display("{0}")]
    Monero(monero::Hash),
    #[display("{0}")]
    Bitcoin(bitcoin::Txid),
}

impl From<monero::Hash> for Txid {
    fn from(t: monero::Hash) -> Txid {
        Txid::Monero(t)
    }
}

impl From<bitcoin::Txid> for Txid {
    fn from(t: bitcoin::Txid) -> Txid {
        Txid::Bitcoin(t)
    }
}

#[derive(Clone, Debug, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
pub struct AddressTransaction {
    pub id: TaskId,
    pub hash: Txid,
    pub amount: u64,
    pub block: Vec<u8>,
    // for bitcoin with bitcoin::consensus encoding, chunked into chunks with
    // length < 2^16 as a workaround for the strict encoding length limit
    pub tx: Vec<Vec<u8>>,
    pub incoming: bool,
}

impl fmt::Display for AddressTransaction {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "AddressTransaction(id: {}, hash: {}, amount: {}, block: {})",
            self.id,
            self.hash,
            self.amount,
            hex::encode(&self.block),
        )
    }
}

#[derive(Clone, Debug, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
pub struct TransactionConfirmations {
    pub id: TaskId,
    pub block: Vec<u8>,
    pub confirmations: Option<u32>,
    // for bitcoin with bitcoin::consensus encoding, chunked into chunks with
    // length < 2^16 as a workaround for the strict encoding length limit
    pub tx: Vec<Vec<u8>>,
}

impl fmt::Display for TransactionConfirmations {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "TransactionConfirmations(id: {}, block: {}, confirmations: {:?})",
            self.id,
            hex::encode(&self.block),
            self.confirmations,
        )
    }
}

#[derive(Clone, Debug, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
pub struct TransactionBroadcasted {
    pub id: TaskId,
    pub tx: Vec<u8>,
    pub error: Option<String>,
}

impl fmt::Display for TransactionBroadcasted {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let bitcoin_tx_id =
            bitcoin::Transaction::consensus_decode(std::io::Cursor::new(self.tx.clone()))
                .map(|tx| tx.txid().to_string())
                .unwrap_or_default();
        write!(
            f,
            "TransactionBroadcasted(id: {}, tx_id: {}, error: {:?})",
            self.id, bitcoin_tx_id, self.error,
        )
    }
}

#[derive(Clone, Debug, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
pub struct SweepSuccess {
    pub id: TaskId,
    pub txids: Vec<Txid>,
}

impl fmt::Display for SweepSuccess {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "SweepSuccess(id: {}, txids: {})",
            self.id,
            self.txids
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<String>>()
                .join(", ")
        )
    }
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct TransactionRetrieved {
    pub id: TaskId,
    pub tx: Option<bitcoin::Transaction>,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct FeeEstimation {
    pub id: TaskId,
    pub fee_estimations: FeeEstimations,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
// the sats per kvB is because we need u64 for Eq, PartialEq and Hash
pub enum FeeEstimations {
    BitcoinFeeEstimation {
        high_priority_sats_per_kvbyte: u64,
        low_priority_sats_per_kvbyte: u64,
    },
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct HealthResult {
    pub id: TaskId,
    pub health: Health,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub enum Health {
    Healthy,
    FaultyElectrum(String),
    FaultyMoneroDaemon(String),
    FaultyMoneroRpcWallet(String),
    ConfigUnavailable(String),
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
// the sats per kvB is because we need u64 for Eq, PartialEq and Hash
pub struct AddressBalance {
    pub id: TaskId,
    pub address: Address,
    pub balance: u64,
    pub err: Option<String>,
}

/// Events returned by syncers to the daemon to update the blockchain states.
/// Events are identified with a unique 32-bits integer that match the [`Task`]
/// id.
#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
pub enum Event {
    /// Notify the daemon the blockchain height changed.
    #[display("{0}")]
    HeightChanged(HeightChanged),
    #[display("{0}")]
    AddressTransaction(AddressTransaction),
    #[display("{0}")]
    TransactionConfirmations(TransactionConfirmations),
    #[display("{0}")]
    TransactionBroadcasted(TransactionBroadcasted),
    #[display("{0}")]
    SweepSuccess(SweepSuccess),
    #[display("{0}")]
    /// Notify the daemon the task has been aborted with success or failure.
    /// Carries the status for the task abortion.
    #[display("{0}")]
    TaskAborted(TaskAborted),
    #[display("{0}")]
    TransactionRetrieved(TransactionRetrieved),
    #[display("{0}")]
    FeeEstimation(FeeEstimation),
    /// Empty event to signify that a task with a certain id has not produced an initial result
    #[display("{0}")]
    Empty(TaskId),
    #[display("{0}")]
    HealthResult(HealthResult),
    #[display("{0}")]
    AddressBalance(AddressBalance),
}
