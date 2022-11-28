use std::io;

use bitcoin::secp256k1::ecdsa::Signature;
use farcaster_core::{
    bitcoin::segwitv0::FundingTx,
    consensus::{self, CanonicalBytes, Decodable, Encodable},
    crypto::dleq::DLEQProof,
    impl_strict_encoding,
    role::TradeRole,
    swap::btcxmr::message::{
        BuyProcedureSignature, CommitAliceParameters, CommitBobParameters, CoreArbitratingSetup,
    },
    swap::btcxmr::{Alice, Bob, EncryptedSignature, KeyManager, Parameters, PublicOffer},
};
use strict_encoding::{StrictDecode, StrictEncode};

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub enum Wallet {
    Alice(AliceState),
    Bob(BobState),
}

#[derive(Clone, Debug)]
pub struct AliceState {
    pub alice: Alice,
    pub local_trade_role: TradeRole,
    pub local_params: Parameters,
    pub key_manager: KeyManager,
    pub pub_offer: PublicOffer,
    pub remote_commit: Option<CommitBobParameters>,
    pub remote_params: Option<Parameters>,
    pub remote_proof: Option<DLEQProof>,
    pub core_arb_setup: Option<CoreArbitratingSetup>,
    pub alice_cancel_signature: Option<Signature>,
    pub adaptor_refund: Option<EncryptedSignature>,
}

impl Encodable for AliceState {
    fn consensus_encode<W: io::Write>(&self, writer: &mut W) -> Result<usize, io::Error> {
        let mut len = self.alice.consensus_encode(writer)?;
        len += self.local_trade_role.consensus_encode(writer)?;
        len += self.local_params.consensus_encode(writer)?;
        len += self.key_manager.consensus_encode(writer)?;
        len += self.pub_offer.consensus_encode(writer)?;
        len += self.remote_commit.consensus_encode(writer)?;
        len += self.remote_params.consensus_encode(writer)?;
        len += self.remote_proof.consensus_encode(writer)?;
        len += self.core_arb_setup.consensus_encode(writer)?;
        len += farcaster_core::consensus::Encodable::consensus_encode(
            &self.alice_cancel_signature.as_canonical_bytes(),
            writer,
        )?;
        len += self.adaptor_refund.consensus_encode(writer)?;
        Ok(len)
    }
}

impl Decodable for AliceState {
    fn consensus_decode<D: io::Read>(d: &mut D) -> Result<Self, consensus::Error> {
        Ok(AliceState {
            alice: Decodable::consensus_decode(d)?,
            local_trade_role: Decodable::consensus_decode(d)?,
            local_params: Decodable::consensus_decode(d)?,
            key_manager: Decodable::consensus_decode(d)?,
            pub_offer: Decodable::consensus_decode(d)?,
            remote_commit: Decodable::consensus_decode(d)?,
            remote_params: Decodable::consensus_decode(d)?,
            remote_proof: Decodable::consensus_decode(d)?,
            core_arb_setup: Decodable::consensus_decode(d)?,
            alice_cancel_signature: Option::<Signature>::from_canonical_bytes(
                farcaster_core::unwrap_vec_ref!(d).as_ref(),
            )?,
            adaptor_refund: Decodable::consensus_decode(d)?,
        })
    }
}

impl_strict_encoding!(AliceState);

impl AliceState {
    pub fn new(
        alice: Alice,
        local_trade_role: TradeRole,
        local_params: Parameters,
        key_manager: KeyManager,
        pub_offer: PublicOffer,
        remote_commit: Option<CommitBobParameters>,
    ) -> Self {
        Self {
            alice,
            local_trade_role,
            local_params,
            key_manager,
            pub_offer,
            remote_commit,
            remote_params: None,
            remote_proof: None,
            core_arb_setup: None,
            alice_cancel_signature: None,
            adaptor_refund: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BobState {
    pub bob: Bob,
    pub local_trade_role: TradeRole,
    pub local_params: Parameters,
    pub key_manager: KeyManager,
    pub pub_offer: PublicOffer,
    pub funding_tx: Option<FundingTx>,
    pub remote_commit: Option<CommitAliceParameters>,
    pub remote_params: Option<Parameters>,
    pub remote_proof: Option<DLEQProof>,
    pub core_arb_setup: Option<CoreArbitratingSetup>,
    pub adaptor_buy: Option<BuyProcedureSignature>,
}

impl Encodable for BobState {
    fn consensus_encode<W: io::Write>(&self, writer: &mut W) -> Result<usize, io::Error> {
        let mut len = self.bob.consensus_encode(writer)?;
        len += self.local_trade_role.consensus_encode(writer)?;
        len += self.local_params.consensus_encode(writer)?;
        len += self.key_manager.consensus_encode(writer)?;
        len += self.pub_offer.consensus_encode(writer)?;
        len += self.funding_tx.consensus_encode(writer)?;
        len += self.remote_commit.consensus_encode(writer)?;
        len += self.remote_params.consensus_encode(writer)?;
        len += self.remote_proof.consensus_encode(writer)?;
        len += self.core_arb_setup.consensus_encode(writer)?;
        len += self.adaptor_buy.consensus_encode(writer)?;
        Ok(len)
    }
}

impl Decodable for BobState {
    fn consensus_decode<D: io::Read>(d: &mut D) -> Result<Self, consensus::Error> {
        Ok(BobState {
            bob: Decodable::consensus_decode(d)?,
            local_trade_role: Decodable::consensus_decode(d)?,
            local_params: Decodable::consensus_decode(d)?,
            key_manager: Decodable::consensus_decode(d)?,
            pub_offer: Decodable::consensus_decode(d)?,
            funding_tx: Decodable::consensus_decode(d)?,
            remote_commit: Decodable::consensus_decode(d)?,
            remote_params: Decodable::consensus_decode(d)?,
            remote_proof: Decodable::consensus_decode(d)?,
            core_arb_setup: Decodable::consensus_decode(d)?,
            adaptor_buy: Decodable::consensus_decode(d)?,
        })
    }
}

impl BobState {
    pub fn new(
        bob: Bob,
        local_trade_role: TradeRole,
        local_params: Parameters,
        key_manager: KeyManager,
        pub_offer: PublicOffer,
        funding_tx: Option<FundingTx>,
        remote_commit: Option<CommitAliceParameters>,
    ) -> Self {
        Self {
            bob,
            local_trade_role,
            local_params,
            key_manager,
            pub_offer,
            funding_tx,
            remote_commit,
            remote_params: None,
            remote_proof: None,
            core_arb_setup: None,
            adaptor_buy: None,
        }
    }
}

impl_strict_encoding!(BobState);
