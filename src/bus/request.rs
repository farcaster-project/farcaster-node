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

#![allow(clippy::clone_on_copy)]

use crate::cli::OfferSelector;
use crate::swapd::CheckpointSwapd;
use crate::syncerd::{Event, SweepBitcoinAddress, SweepMoneroAddress, Task};
use crate::walletd::runtime::CheckpointWallet;
use amplify::{ToYamlString, Wrapper};
use internet2::{CreateUnmarshaller, Unmarshaller};
use lazy_static::lazy_static;
use std::collections::BTreeMap;
use std::fmt::{self, Debug, Display, Formatter};
use std::{iter::FromIterator, str::FromStr};
use uuid::Uuid;

use bitcoin::{
    secp256k1::{
        rand::{thread_rng, RngCore},
    },
    OutPoint, Transaction,
};
use farcaster_core::{
    blockchain::Blockchain,
    swap::btcxmr::{PublicOffer},
    swap::SwapId,
};
use internet2::addr::{InetSocketAddr, NodeAddr};
use internet2::Api;
use microservices::rpc;
use strict_encoding::{StrictDecode, StrictEncode};

use crate::bus::ctl::Ctl;
use crate::bus::msg::Msg;
use crate::bus::rpc::Rpc;

lazy_static! {
    pub static ref UNMARSHALLER: Unmarshaller<Msg> = Msg::create_unmarshaller();
}

#[derive(Eq, PartialEq, Hash, Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("request_id({0})")]
pub struct RequestId(pub u64);

impl RequestId {
    pub fn rand() -> RequestId {
        let mut id = [0u8; 8];
        thread_rng().fill_bytes(&mut id);
        RequestId(u64::from_be_bytes(id))
    }
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode, Eq, PartialEq)]
#[display(format_keys)]
pub struct Keys(
    pub bitcoin::secp256k1::SecretKey,
    pub bitcoin::secp256k1::PublicKey,
);

fn format_keys(keys: &Keys) -> String {
    format!("sk: {}, pk: {}", keys.0.display_secret(), keys.1,)
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display(inner)]
pub enum Tx {
    #[display("lock(..)")]
    Lock(Transaction),
    #[display("buy(..)")]
    Buy(Transaction),
    #[display("funding(..)")]
    Funding(Transaction),
    #[display("cancel(..)")]
    Cancel(Transaction),
    #[display("refund(..)")]
    Refund(Transaction),
    #[display("punish(..)")]
    Punish(Transaction),
}

use crate::{Error, ServiceId};

#[derive(Clone, Debug, Display, From, Api)]
#[api(encoding = "strict")]
#[non_exhaustive]
pub enum Request {
    #[api(type = 5)]
    #[display("msg({0})")]
    Msg(Msg),

    #[api(type = 55)]
    #[display("ctl({0})")]
    Ctl(Ctl),

    #[api(type = 66)]
    #[display(inner)]
    Rpc(Rpc),

    #[api(type = 36)]
    #[display("get_sweep_bitcoin_address({0})")]
    GetSweepBitcoinAddress(bitcoin::Address),

    #[api(type = 28)]
    #[display("keys({0})")]
    Keys(Keys),

    #[api(type = 45)]
    #[display("funding_updated()")]
    FundingUpdated,

    #[api(type = 46)]
    #[display("swap_outcome({0})")]
    SwapOutcome(Outcome),

    #[api(type = 200)]
    #[display("listen({0})")]
    Listen(InetSocketAddr),

    #[api(type = 201)]
    #[display("connect({0})")]
    ConnectPeer(NodeAddr),

    #[api(type = 202)]
    #[display("ping_peer()")]
    PingPeer,

    #[api(type = 196)]
    #[display("transaction({0})")]
    Tx(Tx),

    #[api(type = 195)]
    #[display("bitcoin_address({0})")]
    BitcoinAddress(BitcoinAddress),

    #[api(type = 194)]
    #[display("monero_address({0})")]
    MoneroAddress(MoneroAddress),

    #[api(type = 205)]
    #[display("fund_swap({0})")]
    FundSwap(OutPoint),

    // Responses to CLI
    // ----------------
    #[api(type = 1004)]
    #[display(inner)]
    String(String),

    #[api(type = 206)]
    #[display(inner)]
    MadeOffer(MadeOffer),

    #[api(type = 207)]
    #[display(inner)]
    TookOffer(TookOffer),

    #[api(type = 1098)]
    #[display("public_offer_hex({0})")]
    PublicOfferHex(String),

    #[api(type = 1108)]
    #[display("funding_info({0})")]
    #[from]
    FundingInfo(FundingInfo),

    #[api(type = 1109)]
    #[display("needs_funding({0})")]
    NeedsFunding(Blockchain),

    #[api(type = 1110)]
    #[display("write_text")]
    WriteText(List<String>),

    #[api(type = 1111)]
    #[display("funding_completed({0})")]
    FundingCompleted(Blockchain),

    #[api(type = 1112)]
    #[display("funding_canceled({0})")]
    FundingCanceled(Blockchain),

    // #[api(type = 1203)]
    // #[display("channel_funding({0})", alt = "{0:#}")]
    // #[from]
    // SwapFunding(PubkeyScript),
    // #[api(type = 1300)]
    // #[display("task({0})", alt = "{0:#}")]
    // #[from]
    // CreateTask(u64), // FIXME
    #[api(type = 1300)]
    #[display("syncer_task({0})", alt = "{0:#}")]
    #[from]
    SyncerTask(Task),

    #[api(type = 1301)]
    #[display("syncer_event({0})", alt = "{0:#}")]
    #[from]
    SyncerEvent(Event),

    #[api(type = 1302)]
    #[display("syncer_bridge_ev({0})", alt = "{0:#}")]
    #[from]
    SyncerdBridgeEvent(SyncerdBridgeEvent),

    #[api(type = 1303)]
    #[display("task({0})", alt = "{0:#}")]
    #[from]
    SweepMoneroAddress(SweepMoneroAddress),

    #[api(type = 1304)]
    #[display("checkpoint({0})", alt = "{0:#}")]
    #[from]
    Checkpoint(Checkpoint),

    #[api(type = 1307)]
    #[display("remove_checkpoint")]
    RemoveCheckpoint(SwapId),

    #[api(type = 1310)]
    #[display("task({0})", alt = "{0:#}")]
    #[from]
    SweepBitcoinAddress(SweepBitcoinAddress),

    #[api(type = 1311)]
    #[display("get_address_secret_key({0})")]
    GetAddressSecretKey(Address),

    #[api(type = 1312)]
    #[display("get_addresses({0})")]
    GetAddresses(Blockchain),

    #[api(type = 1313)]
    #[display("bitcoin_address_list({0})")]
    BitcoinAddressList(List<bitcoin::Address>),

    #[api(type = 1318)]
    #[display("monero_address_list({0})")]
    MoneroAddressList(List<String>),

    #[api(type = 1314)]
    #[display("set_address_secret_key")]
    SetAddressSecretKey(AddressSecretKey),

    #[api(type = 1315)]
    #[display("set_offer_history({0})")]
    SetOfferStatus(OfferStatusPair),

    #[api(type = 1316)]
    #[display("retrieve_offers({0})")]
    RetrieveOffers(OfferStatusSelector),

    #[api(type = 1319)]
    #[display("address_secret_key")]
    AddressSecretKey(AddressSecretKey),
}

#[derive(Clone, Debug, Eq, PartialEq, Display, StrictEncode, StrictDecode)]
#[display("{offer}, {status}")]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(OfferStatusPair::to_yaml_string)]
pub struct OfferStatusPair {
    pub offer: PublicOffer,
    pub status: OfferStatus,
}

#[derive(Clone, Debug, Eq, PartialEq, Display, StrictEncode, StrictDecode)]
pub enum OfferStatusSelector {
    #[display("Open")]
    Open,
    #[display("In Progress")]
    InProgress,
    #[display("Ended")]
    Ended,
    #[display("All")]
    All,
}

impl From<OfferSelector> for OfferStatusSelector {
    fn from(offer_selector: OfferSelector) -> OfferStatusSelector {
        match offer_selector {
            OfferSelector::Open => OfferStatusSelector::Open,
            OfferSelector::InProgress => OfferStatusSelector::InProgress,
            OfferSelector::Ended => OfferStatusSelector::Ended,
            OfferSelector::All => OfferStatusSelector::All,
        }
    }
}

impl FromStr for OfferStatusSelector {
    type Err = ();
    fn from_str(input: &str) -> Result<OfferStatusSelector, Self::Err> {
        match input {
            "open" | "Open" => Ok(OfferStatusSelector::Open),
            "in_progress" | "inprogress" => Ok(OfferStatusSelector::Open),
            "ended" | "Ended" => Ok(OfferStatusSelector::Ended),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Display, StrictEncode, StrictDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub enum OfferStatus {
    #[display("Open")]
    Open,
    #[display("In Progress")]
    InProgress,
    #[display("Ended({0})")]
    Ended(Outcome),
}

#[derive(Clone, Debug, Eq, PartialEq, Display, StrictEncode, StrictDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
pub enum Outcome {
    #[display("Success(Swapped)")]
    Buy,
    #[display("Failure(Refunded)")]
    Refund,
    #[display("Failure(Punished)")]
    Punish,
    #[display("Failure(Aborted)")]
    Abort,
}

#[derive(Eq, PartialEq, Clone, Debug, Display, StrictDecode, StrictEncode)]
pub enum Address {
    #[display("{0}")]
    Bitcoin(bitcoin::Address),
    #[display("{0}")]
    Monero(monero::Address),
}

#[derive(Clone, Debug, Display, StrictDecode, StrictEncode)]
pub enum FundingInfo {
    #[display("bitcoin(..)")]
    Bitcoin(BitcoinFundingInfo),
    #[display("monero(..)")]
    Monero(MoneroFundingInfo),
}

#[derive(Clone, Debug, Display, StrictDecode, StrictEncode)]
#[display(Debug)]
pub struct Checkpoint {
    pub swap_id: SwapId,
    pub state: CheckpointState,
}

#[derive(Clone, Debug, Display, StrictDecode, StrictEncode)]
pub enum CheckpointState {
    #[display("Checkpoint Wallet")]
    CheckpointWallet(CheckpointWallet),
    #[display("Checkpoint Swap")]
    CheckpointSwapd(CheckpointSwapd),
}

#[derive(Clone, Debug, Display, StrictDecode, StrictEncode)]
#[display("address_secret_key")]
pub enum AddressSecretKey {
    Bitcoin {
        address: bitcoin::Address,
        secret_key: bitcoin::secp256k1::SecretKey,
    },
    Monero {
        address: monero::Address,
        view: monero::PrivateKey,
        spend: monero::PrivateKey,
    },
}

impl FromStr for BitcoinFundingInfo {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        let content: Vec<&str> = s.split(' ').collect();

        Ok(BitcoinFundingInfo {
            swap_id: SwapId::from_str(content[0])?,
            amount: bitcoin::Amount::from_str(&format!("{} {}", content[2], content[3]))?,
            address: bitcoin::Address::from_str(content[5])?,
        })
    }
}

impl fmt::Display for BitcoinFundingInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:#?} needs {} to {}",
            self.swap_id, self.amount, self.address
        )
    }
}

#[derive(Clone, Debug, StrictDecode, StrictEncode)]
pub struct BitcoinFundingInfo {
    pub swap_id: SwapId,
    pub address: bitcoin::Address,
    pub amount: bitcoin::Amount,
}

impl FromStr for MoneroFundingInfo {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        let content: Vec<&str> = s.split(' ').collect();
        Ok(MoneroFundingInfo {
            swap_id: SwapId::from_str(content[0])?,
            amount: monero::Amount::from_str_with_denomination(&format!(
                "{} {}",
                content[2], content[3]
            ))?,

            address: monero::Address::from_str(content[5])?,
        })
    }
}

impl fmt::Display for MoneroFundingInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:#?} needs {} to {}",
            self.swap_id, self.amount, self.address
        )
    }
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct MoneroFundingInfo {
    pub swap_id: SwapId,
    pub amount: monero::Amount,
    pub address: monero::Address,
}

impl rpc::Request for Request {}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("{source}, {event}")]
pub struct SyncerdBridgeEvent {
    pub event: Event,
    pub source: ServiceId,
}

#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
#[display("{1}")]
pub struct BitcoinAddress(pub SwapId, pub bitcoin::Address);

#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
#[display("{1}")]
pub struct MoneroAddress(pub SwapId, pub monero::Address);

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(MadeOffer::to_yaml_string)]
pub struct MadeOffer {
    pub message: String,
    pub offer_info: OfferInfo,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(OfferInfo::to_yaml_string)]
pub struct OfferInfo {
    pub offer: String,
    pub details: PublicOffer,
}

#[cfg_attr(feature = "serde", serde_as)]
#[derive(Clone, PartialEq, Eq, Debug, Display)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(crate = "serde_crate")
)]
#[display(TookOffer::to_yaml_string)]
pub struct TookOffer {
    pub offerid: Uuid,
    pub message: String,
}

impl StrictEncode for TookOffer {
    fn strict_encode<W: std::io::Write>(&self, mut w: W) -> Result<usize, strict_encoding::Error> {
        let mut len = self.offerid.to_bytes_le().strict_encode(&mut w)?;
        len += self.message.strict_encode(&mut w)?;
        Ok(len)
    }
}

impl StrictDecode for TookOffer {
    fn strict_decode<R: std::io::Read>(mut r: R) -> Result<Self, strict_encoding::Error> {
        let offerid = Uuid::from_bytes_le(<[u8; 16]>::strict_decode(&mut r)?);
        let message = String::strict_decode(&mut r)?;
        Ok(TookOffer { offerid, message })
    }
}

pub type RemotePeerMap<T> = BTreeMap<NodeAddr, T>;

#[cfg(feature = "serde")]
impl ToYamlString for MadeOffer {}
#[cfg(feature = "serde")]
impl ToYamlString for TookOffer {}
#[cfg(feature = "serde")]
impl ToYamlString for OfferStatusPair {}
#[cfg(feature = "serde")]
impl ToYamlString for OfferInfo {}

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
