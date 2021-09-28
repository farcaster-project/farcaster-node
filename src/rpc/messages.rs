use amplify::DumbDefault;
use lnp::ChannelId as SwapId;
use std::collections::HashSet;
use std::fmt::{self, Display, Formatter};
use std::io;

use bitcoin::hashes::{sha256, Hmac};
use bitcoin::secp256k1::{PublicKey, Signature};
use bitcoin::{Script, Txid};
use internet2::{CreateUnmarshaller, Payload, Unmarshall, Unmarshaller};
use lightning_encoding::{self, LightningDecode, LightningEncode};
use lnpbp::chain::AssetId;
use strict_encoding::{self, StrictDecode, StrictEncode};

/// Some features don't make sense on a per-channels or per-node basis, so each
/// feature defines how it is presented in those contexts. Some features may be
/// required for opening a channel, but not a requirement for use of the
/// channel, so the presentation of those features depends on the feature
/// itself.
///
/// # Specification
/// <https://github.com/lightningnetwork/lightning-rfc/blob/master/09-features.md#bolt-9-assigned-feature-flags>
#[derive(Clone, PartialEq, Eq, Hash, Debug, Display)]
pub enum FeatureContext {
    /// `I`: presented in the init message.
    // TODO: Add `alt = doc_comments` when `amplify` crate will support it
    #[display("I")]
    Init,

    /// `N`: N: presented in the node_announcement messages
    #[display("N")]
    NodeAnnouncement,

    /// `C`: presented in the channel_announcement message.
    #[display("C")]
    ChannelAnnouncement,

    /// `9`: presented in BOLT 11 invoices.
    #[display("9")]
    Bolt11Invoice,
}

#[derive(Clone, PartialEq, Eq, Debug, Display, Default)]
#[display("flag<{context:?}, global={global}>, required={required}>")]
pub struct FeatureFlag {
    pub context: HashSet<FeatureContext>,
    pub global: bool,
    pub required: bool,
}

/// Flags are numbered from the least-significant bit, at bit 0 (i.e. 0x1, an
/// even bit). They are generally assigned in pairs so that features can be
/// introduced as optional (odd bits) and later upgraded to be compulsory (even
/// bits), which will be refused by outdated nodes: see BOLT #1: The init
/// Message.
///
/// # Specification
/// <https://github.com/lightningnetwork/lightning-rfc/blob/master/09-features.md>
#[derive(Clone, PartialEq, Eq, Debug, Display, Default)]
#[display(Debug)]
pub struct Features {
    /// Requires or supports extra `channel_reestablish` fields
    // #[lnpwp_feature(0, 1)]
    pub option_data_loss_protect: FeatureFlag,

    /// Sending node needs a complete routing information dump
    // #[lnpwp_feature(3)]
    pub initial_routing_sync: FeatureFlag,

    /// Commits to a shutdown scriptpubkey when opening channel
    // #[lnpwp_feature(4, 5)]
    pub option_upfront_sutdown_script: FeatureFlag,

    /// More sophisticated gossip control
    // #[lnpwp_feature(6, 7)]
    pub gossip_queries: FeatureFlag,

    /// Requires/supports variable-length routing onion payloads
    // #[lnpwp_feature(8, 9)]
    pub var_onion_optin: FeatureFlag,

    /// Gossip queries can include additional information
    // #[lnpwp_feature(10, 11, requires(gossip_queries))]
    pub gossip_queries_ex: FeatureFlag,

    /// Static key for remote output
    // #[lnpwp_feature(12, 13)]
    pub option_static_remotekey: FeatureFlag,

    /// Node supports `payment_secret` field
    // #[lnpwp_feature(14, 15, requires(var_onion_optin))]
    pub payment_secret: FeatureFlag,

    /// Node can receive basic multi-part payments
    // #[lnpwp_feature(16, 17, requires(payment_secret))]
    pub basic_mpp: FeatureFlag,

    /// Can create large channels
    // #[lnpwp_feature(18, 19)]
    pub option_support_large_channel: FeatureFlag,

    /// Anchor outputs
    // #[lnpwp_feature(20, 21, requires(option_static_remotekey))]
    pub option_anchor_outputs: FeatureFlag,
    /* /// FIXME: turn back on Rest of feature flags which are unknown to
     * the current implementation pub unknown: FlagVec, */
}

/// TODO: Implement proper strict encoding for Features

impl StrictEncode for Features {
    fn strict_encode<E: io::Write>(&self, e: E) -> Result<usize, strict_encoding::Error> {
        Ok(0)
    }
}

impl StrictDecode for Features {
    fn strict_decode<D: io::Read>(d: D) -> Result<Self, strict_encoding::Error> {
        Ok(none!())
    }
}

impl LightningEncode for Features {
    fn lightning_encode<E: io::Write>(&self, e: E) -> Result<usize, lightning_encoding::Error> {
        Ok(0)
    }
}

impl LightningDecode for Features {
    fn lightning_decode<D: io::Read>(d: D) -> Result<Self, lightning_encoding::Error> {
        Ok(none!())
    }
}

// #[display("init({global_features}, {local_features}, {assets:#?})")]
#[derive(Clone, Debug, From, StrictDecode, StrictEncode)]
pub struct Init {
    pub global_features: Features,
    pub local_features: Features,
    // #[tlv(type = 1)]
    pub assets: HashSet<AssetId>,
    /* #[tlv(unknown)]
     * pub unknown_tlvs: BTreeMap<tlv::Type, tlv::RawRecord>, */
}

#[derive(Clone, Debug, From, StrictDecode, StrictEncode)]
pub enum FarMsgs {
    // Part I: Generic messages outside of channel operations
    // ======================================================
    /// Once authentication is complete, the first message reveals the features
    /// supported or required by this node, even if this is a reconnection.
    // #[lnp_api(type = 16)]
    // #[display(inner)]
    Init(Init),

    /// For simplicity of diagnosis, it's often useful to tell a peer that
    /// something is incorrect.
    // #[lnp_api(type = 17)]
    // #[display(inner)]
    Error(Error),

    /// In order to allow for the existence of long-lived TCP connections, at
    /// times it may be required that both ends keep alive the TCP connection
    /// at the application level. Such messages also allow obfuscation of
    /// traffic patterns.
    // #[lnp_api(type = 18)]
    // #[display(inner)]
    Ping(Ping),

    /// The pong message is to be sent whenever a ping message is received. It
    /// serves as a reply and also serves to keep the connection alive, while
    /// explicitly notifying the other end that the receiver is still active.
    /// Within the received ping message, the sender will specify the number of
    /// bytes to be included within the data payload of the pong message.
    // #[lnp_api(type = 19)]
    // #[display("pong(...)")]
    Pong(Vec<u8>),
}

/// In order to allow for the existence of long-lived TCP connections, at
/// times it may be required that both ends keep alive the TCP connection
/// at the application level. Such messages also allow obfuscation of
/// traffic patterns.
///
/// # Specification
/// <https://github.com/lightningnetwork/lightning-rfc/blob/master/01-messaging.md#the-ping-and-pong-messages>
#[derive(
    Clone,
    PartialEq,
    Eq,
    Debug,
    Display,
    LightningEncode,
    LightningDecode,
    StrictEncode,
    StrictDecode,
)]
#[display(Debug)]
pub struct Ping {
    pub ignored: Vec<u8>,
    pub pong_size: u16,
}

/// For simplicity of diagnosis, it's often useful to tell a peer that something
/// is incorrect.
///
/// # Specification
/// <https://github.com/lightningnetwork/lightning-rfc/blob/master/01-messaging.md#the-error-message>
#[derive(
    Clone,
    PartialEq,
    Debug,
    // Error,
    LightningEncode,
    LightningDecode,
    StrictEncode,
    StrictDecode,
)]
pub struct Error {
    /// The channel is referred to by channel_id, unless channel_id is 0 (i.e.
    /// all bytes are 0), in which case it refers to all channels.
    pub swap_id: SwapId,

    /// Any specific error details, either as string or binary data
    pub data: Vec<u8>,
}
