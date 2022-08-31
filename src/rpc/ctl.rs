use farcaster_core::{
    swap::btcxmr::{Offer, PublicOffer},
    swap::SwapId,
};

use internet2::addr::InetSocketAddr;
use internet2::Api;
use strict_encoding::{StrictDecode, StrictEncode};

#[derive(Clone, Debug, Display, From, StrictDecode, StrictEncode, Api)]
#[api(encoding = "strict")]
#[non_exhaustive]
pub enum Ctl {
    #[api(type = 1309)]
    #[display("restore_checkpoint({0})", alt = "{0:#}")]
    RestoreCheckpoint(SwapId),

    #[api(type = 198)]
    #[display("make_offer({0})")]
    MakeOffer(ProtoPublicOffer),

    #[api(type = 199)]
    #[display("take_offer({0}))")]
    TakeOffer(PubOffer),
}

#[derive(Clone, PartialEq, Eq, Debug, Display, StrictEncode, StrictDecode)]
#[display("..")]
pub struct ProtoPublicOffer {
    pub offer: Offer,
    pub public_addr: InetSocketAddr,
    pub bind_addr: InetSocketAddr,
    pub arbitrating_addr: bitcoin::Address,
    pub accordant_addr: monero::Address,
}

#[derive(Clone, Debug, Display, StrictEncode, StrictDecode)]
#[display("{public_offer}, ..")]
pub struct PubOffer {
    pub public_offer: PublicOffer,
    pub external_address: bitcoin::Address,
    pub internal_address: monero::Address,
}
