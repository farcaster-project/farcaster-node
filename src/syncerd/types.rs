#[cfg(feature = "serde")]
use serde_with::DisplayFromStr;
use strict_encoding::{StrictDecode, StrictEncode};

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
#[display(Debug)]
pub enum AddressAddendum {
    Monero(XmrAddressAddendum),
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
#[display(Debug)]
pub struct XmrAddressAddendum {
    #[serde_as(as = "DisplayFromStr")]
    pub spend_key: monero::PublicKey,
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
#[display(Debug)]
pub struct SweepAddress {
    pub retry: bool,
    pub id: TaskId,
    pub lifetime: u64,
    pub addendum: SweepAddressAddendum,
    pub from_height: Option<u64>,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub enum SweepAddressAddendum {
    Monero(SweepMoneroAddress),
    Bitcoin(SweepBitcoinAddress),
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, Debug, Display, Eq, PartialEq, Hash, StrictEncode, StrictDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub struct SweepMoneroAddress {
    #[serde_as(as = "DisplayFromStr")]
    pub source_spend_key: monero::PrivateKey,
    #[serde_as(as = "DisplayFromStr")]
    pub source_view_key: monero::PrivateKey,
    pub destination_address: monero::Address,
    #[serde(with = "monero::util::amount::serde::as_xmr")]
    pub minimum_balance: monero::Amount,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
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
#[display(Debug)]
pub struct WatchAddress {
    pub id: TaskId,
    pub lifetime: u64,
    pub addendum: AddressAddendum,
    pub include_tx: Boolean,
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

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub struct WatchTransaction {
    pub id: TaskId,
    pub lifetime: u64,
    #[serde(with = "hex")]
    pub hash: Vec<u8>,
    pub confirmation_bound: u32,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub struct BroadcastTransaction {
    pub id: TaskId,
    #[serde(with = "hex")]
    pub tx: Vec<u8>,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub struct GetTx {
    pub id: TaskId,
    #[serde(with = "hex")]
    pub hash: Vec<u8>,
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

/// Tasks created by the daemon and handle by syncers to process a blockchain
/// and generate [`Event`] back to the syncer.
#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(Debug)]
pub enum Task {
    Abort(Abort),
    WatchHeight(WatchHeight),
    WatchAddress(WatchAddress),
    WatchTransaction(WatchTransaction),
    BroadcastTransaction(BroadcastTransaction),
    SweepAddress(SweepAddress),
    GetTx(GetTx),
    WatchEstimateFee(WatchEstimateFee),
    Terminate,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct TaskAborted {
    pub id: Vec<TaskId>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct HeightChanged {
    pub id: TaskId,
    pub block: Vec<u8>,
    pub height: u64,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct AddressTransaction {
    pub id: TaskId,
    pub hash: Vec<u8>,
    pub amount: u64,
    pub block: Vec<u8>,
    // for bitcoin with bitcoin::consensus encoding
    pub tx: Vec<u8>,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct TransactionConfirmations {
    pub id: TaskId,
    pub block: Vec<u8>,
    pub confirmations: Option<u32>,
    // for bitcoin with bitcoin::consensus encoding
    pub tx: Vec<u8>,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct TransactionBroadcasted {
    pub id: TaskId,
    pub tx: Vec<u8>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct SweepSuccess {
    pub id: TaskId,
    pub txids: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq, Hash)]
#[display(Debug)]
pub struct TransactionRetrieved {
    pub id: TaskId,
    // for bitcoin with bitcoin::consensus encoding
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
    TransactionRetrieved(TransactionRetrieved),
    FeeEstimation(FeeEstimation),
}
