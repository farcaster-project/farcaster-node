// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use std::{convert::TryInto, io};

use bitcoin::secp256k1::ecdsa::Signature;
use farcaster_core::{
    bitcoin::{
        segwitv0::{BuyTx, CancelTx, FundingTx, LockTx, PunishTx, RefundTx},
        BitcoinSegwitV0,
    },
    blockchain::FeePriority,
    consensus::{self, CanonicalBytes, Decodable, Encodable},
    crypto::{
        dleq::DLEQProof, ArbitratingKeyId, CommitmentEngine, GenerateKey, ProveCrossGroupDleq,
        SharedKeyId,
    },
    impl_strict_encoding,
    monero::{Monero, SHARED_VIEW_KEY_ID},
    role::{SwapRole, TradeRole},
    swap::btcxmr::{
        message::{RefundProcedureSignatures, RevealProof},
        Alice, Bob, Deal, EncryptedSignature, KeyManager, Parameters,
    },
    swap::{
        btcxmr::{
            message::{
                BuyProcedureSignature, CommitAliceParameters, CommitBobParameters,
                CoreArbitratingSetup,
            },
            FullySignedPunish, TxSignatures,
        },
        SwapId,
    },
    transaction::{Broadcastable, Fundable, Transaction, Witnessable},
};

use strict_encoding::{StrictDecode, StrictEncode};

use crate::{
    bus::{
        self,
        ctl::{CtlMsg, Tx},
        p2p::{Commit, Reveal},
        AddressSecretKey, BitcoinSecretKeyInfo, BusMsg, MoneroSecretKeyInfo, ServiceBus,
    },
    syncerd::{SweepBitcoinAddress, SweepMoneroAddress},
    Endpoints, Error, LogStyle, ServiceId,
};

pub struct HandleRefundProcedureSignaturesRes {
    pub buy_procedure_signature: BuyProcedureSignature,
    pub lock_tx: bitcoin::Transaction,
    pub cancel_tx: bitcoin::Transaction,
    pub refund_tx: bitcoin::Transaction,
}

pub struct HandleCoreArbitratingSetupRes {
    pub refund_procedure_signatures: RefundProcedureSignatures,
    pub cancel_tx: bitcoin::Transaction,
    pub punish_tx: bitcoin::Transaction,
}

pub struct HandleBuyProcedureSignatureRes {
    pub cancel_tx: bitcoin::Transaction,
    pub buy_tx: bitcoin::Transaction,
}

#[derive(Clone, Display, Debug, StrictEncode, StrictDecode)]
pub enum Wallet {
    #[display("Alice's wallet")]
    Alice(AliceState),
    #[display("Bob's wallet")]
    Bob(BobState),
}

#[derive(Clone, Debug)]
pub struct AliceState {
    pub alice: Alice,
    pub local_trade_role: TradeRole,
    pub local_params: Parameters,
    pub key_manager: KeyManager,
    pub deal: Deal,
    pub remote_commit: Option<CommitBobParameters>,
    pub remote_params: Option<Parameters>,
    pub remote_proof: Option<DLEQProof>,
    pub core_arb_setup: Option<CoreArbitratingSetup>,
    pub alice_cancel_signature: Option<Signature>,
    pub adaptor_refund: Option<EncryptedSignature>,
    pub target_bitcoin_address: bitcoin::Address,
    pub target_monero_address: monero::Address,
}

impl Encodable for AliceState {
    fn consensus_encode<W: io::Write>(&self, writer: &mut W) -> Result<usize, io::Error> {
        let mut len = self.alice.consensus_encode(writer)?;
        len += self.local_trade_role.consensus_encode(writer)?;
        len += self.local_params.consensus_encode(writer)?;
        len += self.key_manager.consensus_encode(writer)?;
        len += self.deal.consensus_encode(writer)?;
        len += self.remote_commit.consensus_encode(writer)?;
        len += self.remote_params.consensus_encode(writer)?;
        len += self.remote_proof.consensus_encode(writer)?;
        len += self.core_arb_setup.consensus_encode(writer)?;
        len += farcaster_core::consensus::Encodable::consensus_encode(
            &self.alice_cancel_signature.as_canonical_bytes(),
            writer,
        )?;
        len += self.adaptor_refund.consensus_encode(writer)?;
        len += self
            .target_bitcoin_address
            .as_canonical_bytes()
            .consensus_encode(writer)?;
        len += self
            .target_monero_address
            .as_canonical_bytes()
            .consensus_encode(writer)?;
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
            deal: Decodable::consensus_decode(d)?,
            remote_commit: Decodable::consensus_decode(d)?,
            remote_params: Decodable::consensus_decode(d)?,
            remote_proof: Decodable::consensus_decode(d)?,
            core_arb_setup: Decodable::consensus_decode(d)?,
            alice_cancel_signature: Option::<Signature>::from_canonical_bytes(
                farcaster_core::unwrap_vec_ref!(d).as_ref(),
            )?,
            adaptor_refund: Decodable::consensus_decode(d)?,
            target_bitcoin_address: bitcoin::Address::from_canonical_bytes(
                farcaster_core::unwrap_vec_ref!(d).as_ref(),
            )?,
            target_monero_address: monero::Address::from_canonical_bytes(
                farcaster_core::unwrap_vec_ref!(d).as_ref(),
            )?,
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
        deal: Deal,
        remote_commit: Option<CommitBobParameters>,
        target_bitcoin_address: bitcoin::Address,
        target_monero_address: monero::Address,
    ) -> Self {
        Self {
            alice,
            local_trade_role,
            local_params,
            key_manager,
            deal,
            remote_commit,
            remote_params: None,
            remote_proof: None,
            core_arb_setup: None,
            alice_cancel_signature: None,
            adaptor_refund: None,
            target_bitcoin_address,
            target_monero_address,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BobState {
    pub bob: Bob,
    pub local_trade_role: TradeRole,
    pub local_params: Parameters,
    pub key_manager: KeyManager,
    pub deal: Deal,
    pub funding_tx: FundingTx,
    pub remote_commit: Option<CommitAliceParameters>,
    pub remote_params: Option<Parameters>,
    pub remote_proof: Option<DLEQProof>,
    pub core_arb_setup: Option<CoreArbitratingSetup>,
    pub adaptor_buy: Option<BuyProcedureSignature>,
    pub target_bitcoin_address: bitcoin::Address,
    pub target_monero_address: monero::Address,
}

impl Encodable for BobState {
    fn consensus_encode<W: io::Write>(&self, writer: &mut W) -> Result<usize, io::Error> {
        let mut len = self.bob.consensus_encode(writer)?;
        len += self.local_trade_role.consensus_encode(writer)?;
        len += self.local_params.consensus_encode(writer)?;
        len += self.key_manager.consensus_encode(writer)?;
        len += self.deal.consensus_encode(writer)?;
        len += self.funding_tx.consensus_encode(writer)?;
        len += self.remote_commit.consensus_encode(writer)?;
        len += self.remote_params.consensus_encode(writer)?;
        len += self.remote_proof.consensus_encode(writer)?;
        len += self.core_arb_setup.consensus_encode(writer)?;
        len += self.adaptor_buy.consensus_encode(writer)?;
        len += self
            .target_bitcoin_address
            .as_canonical_bytes()
            .consensus_encode(writer)?;
        len += self
            .target_monero_address
            .as_canonical_bytes()
            .consensus_encode(writer)?;
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
            deal: Decodable::consensus_decode(d)?,
            funding_tx: Decodable::consensus_decode(d)?,
            remote_commit: Decodable::consensus_decode(d)?,
            remote_params: Decodable::consensus_decode(d)?,
            remote_proof: Decodable::consensus_decode(d)?,
            core_arb_setup: Decodable::consensus_decode(d)?,
            adaptor_buy: Decodable::consensus_decode(d)?,
            target_bitcoin_address: bitcoin::Address::from_canonical_bytes(
                farcaster_core::unwrap_vec_ref!(d).as_ref(),
            )?,
            target_monero_address: monero::Address::from_canonical_bytes(
                farcaster_core::unwrap_vec_ref!(d).as_ref(),
            )?,
        })
    }
}

impl BobState {
    pub fn new(
        bob: Bob,
        local_trade_role: TradeRole,
        local_params: Parameters,
        key_manager: KeyManager,
        deal: Deal,
        funding_tx: FundingTx,
        remote_commit: Option<CommitAliceParameters>,
        target_bitcoin_address: bitcoin::Address,
        target_monero_address: monero::Address,
    ) -> Self {
        Self {
            bob,
            local_trade_role,
            local_params,
            key_manager,
            deal,
            funding_tx,
            remote_commit,
            remote_params: None,
            remote_proof: None,
            core_arb_setup: None,
            adaptor_buy: None,
            target_bitcoin_address,
            target_monero_address,
        }
    }
}

impl_strict_encoding!(BobState);

impl Wallet {
    pub fn local_params(&self) -> bus::ctl::Params {
        match self {
            Wallet::Alice(AliceState { local_params, .. }) => {
                bus::ctl::Params::Alice(local_params.clone())
            }
            Wallet::Bob(BobState { local_params, .. }) => {
                bus::ctl::Params::Bob(local_params.clone())
            }
        }
    }

    pub fn funding_address(&self) -> Option<bitcoin::Address> {
        match self {
            Wallet::Alice(..) => None,
            Wallet::Bob(BobState { funding_tx, .. }) => funding_tx.get_address().ok(),
        }
    }

    pub fn new_taker(
        endpoints: &mut Endpoints,
        deal: Deal,
        target_bitcoin_address: bitcoin::Address,
        target_monero_address: monero::Address,
        mut key_manager: KeyManager,
        swap_id: SwapId,
    ) -> Result<Self, Error> {
        let Deal {
            parameters: deal_parameters,
            ..
        } = deal.clone();

        // since we're takers, we are on the other side of the trade
        let taker_role = deal_parameters.maker_role.other();
        match taker_role {
            SwapRole::Bob => {
                let bob = Bob::new(
                    BitcoinSegwitV0::new(),
                    Monero,
                    target_bitcoin_address.clone(),
                    FeePriority::Low,
                );
                let local_params = bob.generate_parameters(&mut key_manager, &deal)?;
                let funding = create_funding(&mut key_manager, deal_parameters.network)?;
                let funding_addr = funding.get_address()?;
                endpoints.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Swap(swap_id),
                    ServiceId::Database,
                    BusMsg::Ctl(CtlMsg::SetAddressSecretKey(AddressSecretKey::Bitcoin {
                        address: funding_addr,
                        secret_key_info: BitcoinSecretKeyInfo {
                            swap_id: Some(swap_id),
                            secret_key: key_manager
                                .get_or_derive_bitcoin_key(ArbitratingKeyId::Lock)?,
                        },
                    })),
                )?;
                info!("{} | Loading {}", swap_id.swap_id(), "Wallet::Bob".label());
                let local_wallet = BobState::new(
                    bob,
                    TradeRole::Taker,
                    local_params,
                    key_manager,
                    deal.clone(),
                    funding,
                    None,
                    target_bitcoin_address,
                    target_monero_address,
                );
                Ok(Wallet::Bob(local_wallet))
            }
            SwapRole::Alice => {
                let alice = Alice::new(
                    BitcoinSegwitV0::new(),
                    Monero,
                    target_bitcoin_address.clone(),
                    FeePriority::Low,
                );
                let local_params = alice.generate_parameters(&mut key_manager, &deal)?;
                let local_wallet = AliceState::new(
                    alice,
                    TradeRole::Taker,
                    local_params,
                    key_manager,
                    deal.clone(),
                    None,
                    target_bitcoin_address,
                    target_monero_address,
                );
                Ok(Wallet::Alice(local_wallet))
            }
        }
    }

    pub fn new_maker(
        endpoints: &mut Endpoints,
        deal: Deal,
        target_bitcoin_address: bitcoin::Address,
        target_monero_address: monero::Address,
        mut key_manager: KeyManager,
        swap_id: SwapId,
        remote_commit: Commit,
    ) -> Result<Self, Error> {
        let Deal {
            parameters: deal_parameters,
            ..
        } = deal.clone();
        match deal_parameters.maker_role {
            SwapRole::Bob => {
                let bob = Bob::new(
                    BitcoinSegwitV0::new(),
                    Monero,
                    target_bitcoin_address.clone(),
                    FeePriority::Low,
                );
                let local_params = bob.generate_parameters(&mut key_manager, &deal)?;
                let funding = create_funding(&mut key_manager, deal_parameters.network)?;
                let funding_addr = funding.get_address()?;
                endpoints.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Swap(swap_id),
                    ServiceId::Database,
                    BusMsg::Ctl(CtlMsg::SetAddressSecretKey(AddressSecretKey::Bitcoin {
                        address: funding_addr,
                        secret_key_info: BitcoinSecretKeyInfo {
                            swap_id: Some(swap_id),
                            secret_key: key_manager
                                .get_or_derive_bitcoin_key(ArbitratingKeyId::Lock)?,
                        },
                    })),
                )?;
                info!("{} | Loading {}", swap_id.swap_id(), "Wallet::Bob".label());
                let local_wallet = if let Commit::AliceParameters(remote_commit) = remote_commit {
                    BobState::new(
                        bob,
                        TradeRole::Maker,
                        local_params,
                        key_manager,
                        deal.clone(),
                        funding,
                        Some(remote_commit),
                        target_bitcoin_address,
                        target_monero_address,
                    )
                } else {
                    error!("{} | Not Commit::Alice", swap_id.swap_id());
                    return Err(Error::Farcaster("Not Commit::Alice".to_string()));
                };
                Ok(Wallet::Bob(local_wallet))
            }
            SwapRole::Alice => {
                let alice = Alice::new(
                    BitcoinSegwitV0::new(),
                    Monero,
                    target_bitcoin_address.clone(),
                    FeePriority::Low,
                );
                let local_params = alice.generate_parameters(&mut key_manager, &deal)?;
                info!(
                    "{} | Loading {}",
                    swap_id.swap_id(),
                    "Wallet::Alice".label()
                );
                let local_wallet = if let Commit::BobParameters(bob_commit) = remote_commit {
                    let local_trade_role = TradeRole::Maker;
                    AliceState::new(
                        alice,
                        local_trade_role,
                        local_params,
                        key_manager,
                        deal.clone(),
                        Some(bob_commit),
                        target_bitcoin_address,
                        target_monero_address,
                    )
                } else {
                    error!("{} | Not Commit::Bob", swap_id.swap_id());
                    return Err(Error::Farcaster("Not Commit::Bob".to_string()));
                };
                Ok(Wallet::Alice(local_wallet))
            }
        }
    }

    pub fn process_funding_tx(&mut self, tx: Tx, swap_id: SwapId) -> Result<(), Error> {
        if let Tx::Funding(tx) = tx {
            if let Wallet::Bob(BobState { funding_tx, .. }) = self {
                if funding_tx.was_seen() {
                    warn!(
                        "{} | Funding was previously updated, ignoring",
                        swap_id.swap_id(),
                    );
                    return Err(Error::Farcaster("Funding already updated".to_string()));
                }
                funding_update(funding_tx, tx)?;
                debug!(
                    "{} | bob's wallet informs swapd that funding was successfully updated",
                    swap_id,
                );
                Ok(())
            } else {
                Err(Error::Farcaster(
                    "Processing funding tx requires a Bob wallet".to_string(),
                ))
            }
        } else {
            Err(Error::Farcaster(
                "Process funding tx only processes funding transactions".to_string(),
            ))
        }
    }

    pub fn process_buy_tx(
        &mut self,
        buy_tx: bitcoin::Transaction,
        endpoints: &mut Endpoints,
        swap_id: SwapId,
        acc_lock_height_lower_bound: Option<u64>,
    ) -> Result<SweepMoneroAddress, Error> {
        if let Wallet::Bob(BobState {
            bob,
            local_params,
            key_manager,
            remote_params: Some(alice_params),
            adaptor_buy: Some(adaptor_buy),
            deal,
            target_monero_address,
            ..
        }) = self
        {
            let sk_a_btc = bob.recover_accordant_key(
                key_manager,
                alice_params,
                adaptor_buy.buy_adaptor_sig.clone(),
                buy_tx,
            );
            let mut sk_a_btc_buf: Vec<u8> = (*sk_a_btc.as_ref()).into();
            sk_a_btc_buf.reverse();
            let sk_a = monero::PrivateKey::from_slice(sk_a_btc_buf.as_ref())
                .expect("Valid Monero Private Key");
            info!(
                "{} | Extracted monero key from Buy tx: {}",
                swap_id.swap_id(),
                sk_a.label()
            );
            let sk_b = key_manager.get_or_derive_monero_spend_key()?;
            let spend = sk_a + sk_b;
            info!(
                "{} | Full secret monero spending key: {}",
                swap_id.swap_id(),
                spend.bright_green_bold()
            );
            let view_key_alice = *alice_params
                .accordant_shared_keys
                .clone()
                .into_iter()
                .find(|vk| vk.tag() == &SharedKeyId::new(SHARED_VIEW_KEY_ID))
                .unwrap()
                .elem();

            let view_key_bob = *local_params
                .accordant_shared_keys
                .clone()
                .into_iter()
                .find(|vk| vk.tag() == &SharedKeyId::new(SHARED_VIEW_KEY_ID))
                .unwrap()
                .elem();
            let view = view_key_alice + view_key_bob;
            info!(
                "{} | Full secret monero view key: {}",
                swap_id.swap_id(),
                view.bright_green_bold()
            );
            let network = deal.parameters.network.into();
            let keypair = monero::KeyPair { view, spend };
            let corresponding_address = monero::Address::from_keypair(network, &keypair);
            info!(
                "{} | Corresponding address: {}",
                swap_id.swap_id(),
                corresponding_address.addr()
            );

            endpoints.send_to(
                ServiceBus::Ctl,
                ServiceId::Swap(swap_id),
                ServiceId::Database,
                BusMsg::Ctl(CtlMsg::SetAddressSecretKey(AddressSecretKey::Monero {
                    address: corresponding_address,
                    secret_key_info: MoneroSecretKeyInfo {
                        swap_id: Some(swap_id),
                        view: keypair.view.as_bytes().try_into().unwrap(),
                        spend: keypair.spend.as_bytes().try_into().unwrap(),
                        creation_height: acc_lock_height_lower_bound,
                    },
                })),
            )?;

            let sweep_keys = SweepMoneroAddress {
                source_view_key: view,
                source_spend_key: spend,
                destination_address: *target_monero_address,
                minimum_balance: deal.parameters.accordant_amount,
                from_height: acc_lock_height_lower_bound,
            };
            Ok(sweep_keys)
        } else {
            Err(Error::Farcaster("Wallet in invalid state".to_string()))
        }
    }

    pub fn process_refund_tx(
        &mut self,
        endpoints: &mut Endpoints,
        refund_tx: bitcoin::Transaction,
        swap_id: SwapId,
        acc_lock_height_lower_bound: Option<u64>,
    ) -> Result<SweepMoneroAddress, Error> {
        if let Wallet::Alice(AliceState {
            alice,
            local_params,
            key_manager,
            remote_params: Some(bob_params), //remote
            adaptor_refund: Some(adaptor_refund),
            deal,
            target_monero_address,
            ..
        }) = self
        {
            let sk_b_btc = alice.recover_accordant_key(
                key_manager,
                bob_params,
                adaptor_refund.clone(),
                refund_tx,
            );
            let mut sk_b_btc_buf: Vec<u8> = (*sk_b_btc.as_ref()).into();
            sk_b_btc_buf.reverse();
            let sk_b = monero::PrivateKey::from_slice(sk_b_btc_buf.as_ref())
                .expect("Valid Monero Private Key");
            info!(
                "{} | Extracted monero key from Refund tx: {}",
                swap_id.swap_id(),
                sk_b.label()
            );

            let sk_a = key_manager.get_or_derive_monero_spend_key()?;
            let spend = sk_a + sk_b;
            info!(
                "{} | Full secret monero spending key: {}",
                swap_id.swap_id(),
                spend.bright_green_bold()
            );

            let view_key_bob = *bob_params
                .accordant_shared_keys
                .clone()
                .into_iter()
                .find(|vk| vk.tag() == &SharedKeyId::new(SHARED_VIEW_KEY_ID))
                .unwrap()
                .elem();

            let view_key_alice = *local_params
                .accordant_shared_keys
                .clone()
                .into_iter()
                .find(|vk| vk.tag() == &SharedKeyId::new(SHARED_VIEW_KEY_ID))
                .unwrap()
                .elem();
            let view = view_key_alice + view_key_bob;
            info!(
                "{} | Full secret monero view key: {}",
                swap_id.swap_id(),
                view.bright_green_bold()
            );
            let network = deal.parameters.network.into();
            let keypair = monero::KeyPair { view, spend };
            let corresponding_address = monero::Address::from_keypair(network, &keypair);
            info!(
                "{} | Corresponding address: {}",
                swap_id.swap_id(),
                corresponding_address.addr()
            );

            endpoints.send_to(
                ServiceBus::Ctl,
                ServiceId::Swap(swap_id),
                ServiceId::Database,
                BusMsg::Ctl(CtlMsg::SetAddressSecretKey(AddressSecretKey::Monero {
                    address: corresponding_address,
                    secret_key_info: MoneroSecretKeyInfo {
                        swap_id: Some(swap_id),
                        view: keypair.view.as_bytes().try_into().unwrap(),
                        spend: keypair.spend.as_bytes().try_into().unwrap(),
                        creation_height: acc_lock_height_lower_bound,
                    },
                })),
            )?;

            let sweep_keys = SweepMoneroAddress {
                source_view_key: view,
                source_spend_key: spend,
                destination_address: *target_monero_address,
                minimum_balance: deal.parameters.accordant_amount,
                from_height: acc_lock_height_lower_bound,
            };
            Ok(sweep_keys)
        } else {
            error!("Call to refund transaction expects an Alice wallet");
            Err(Error::Farcaster(
                "Refund transaction requires an Alice wallet".to_string(),
            ))
        }
    }

    pub fn process_get_sweep_bitcoin_address(
        &mut self,
        source_address: bitcoin::Address,
        swap_id: SwapId,
    ) -> Result<SweepBitcoinAddress, Error> {
        if let Wallet::Bob(BobState {
            key_manager, bob, ..
        }) = self
        {
            let source_secret_key =
                key_manager.get_or_derive_bitcoin_key(ArbitratingKeyId::Lock)?;
            let destination_address = bob.refund_address.clone();
            Ok(SweepBitcoinAddress {
                source_secret_key,
                source_address,
                destination_address,
            })
        } else {
            error!("{} | get funding key requires a bob wallet", swap_id);
            Err(Error::Farcaster(
                "Funding key requires a Bob wallet".to_string(),
            ))
        }
    }

    pub fn handle_maker_commit(
        &mut self,
        commit: Commit,
        swap_id: SwapId,
    ) -> Result<Reveal, Error> {
        match commit {
            Commit::BobParameters(commit) => {
                if let Wallet::Alice(AliceState {
                    remote_commit, // None
                    ..
                }) = self
                {
                    if remote_commit.is_some() {
                        error!("{} | Bob Commit (remote) already set", swap_id.swap_id());
                    } else {
                        trace!("Setting Bob Commit");
                        *remote_commit = Some(commit);
                    }
                } else {
                    error!(
                        "{} | Wallet not found or not on correct state",
                        swap_id.swap_id(),
                    );
                    return Err(Error::Farcaster(
                        "Needs to be an Alice wallet to process Bob Commit".to_string(),
                    ));
                }
            }
            Commit::AliceParameters(commit) => {
                if let Wallet::Bob(BobState {
                    remote_commit, // None
                    ..
                }) = self
                {
                    if remote_commit.is_some() {
                        error!("{} | Alice Commit (remote) already set", swap_id.swap_id());
                    } else {
                        trace!("Setting Alice Commit");
                        *remote_commit = Some(commit);
                    }
                } else {
                    error!(
                        "{} | Not correct wallet, should be Bob's wallet: {}",
                        swap_id.swap_id(),
                        self
                    );
                    return Err(Error::Farcaster(
                        "Needs to be a Bob wallet to process Alice Commit".to_string(),
                    ));
                }
            }
        }
        // craft the correct reveal depending on role
        let reveal = match self {
            Wallet::Alice(AliceState { local_params, .. }) => Reveal::Alice {
                parameters: local_params.clone().reveal_alice(swap_id),
                proof: RevealProof {
                    swap_id,
                    proof: local_params.proof.clone().expect("local always some"),
                },
            },
            Wallet::Bob(BobState { local_params, .. }) => Reveal::Bob {
                parameters: local_params.clone().reveal_bob(swap_id),
                proof: RevealProof {
                    swap_id,
                    proof: local_params.proof.clone().expect("local always some"),
                },
            },
        };
        Ok(reveal)
    }

    pub fn handle_bob_reveals(
        &mut self,
        reveal: Reveal,
        swap_id: SwapId,
    ) -> Result<Option<Reveal>, Error> {
        match reveal {
            // receiving from counterparty Bob, thus wallet is acting as Alice (Maker or
            // Taker)
            Reveal::Bob { parameters, proof } => {
                if let Wallet::Alice(AliceState {
                    local_params,
                    key_manager,
                    deal,
                    remote_commit: Some(remote_commit),
                    remote_params, // None
                    remote_proof,  // None
                    ..
                }) = self
                {
                    if remote_params.is_some() || remote_proof.is_some() {
                        error!("{} | Bob params or proof already set", swap_id.swap_id(),);
                        return Err(Error::Farcaster(
                            "Bob params or proof already set".to_string(),
                        ));
                    }

                    trace!("Setting Bob params: {}", parameters);
                    trace!("Setting Bob proof: {}", proof);
                    remote_commit.verify_with_reveal(&CommitmentEngine, parameters.clone())?;
                    let remote_params_candidate: Parameters = parameters.into_parameters();
                    let proof_verification = key_manager.verify_proof(
                        &remote_params_candidate.spend,
                        &remote_params_candidate.adaptor,
                        proof.proof.clone(),
                    );
                    if proof_verification.is_err() {
                        error!("{} | DLEQ proof invalid", swap_id.swap_id());
                        return Err(Error::Farcaster("DLEQ invalid".to_string()));
                    }
                    info!("{} | Proof successfully verified", swap_id.swap_id());
                    *remote_params = Some(remote_params_candidate);
                    *remote_proof = Some(proof.proof);
                    // if we're maker, send Reveal back to counterparty
                    if deal.swap_role(&TradeRole::Maker) == SwapRole::Alice {
                        Ok(Some(Reveal::Alice {
                            parameters: local_params.clone().reveal_alice(swap_id),
                            proof: RevealProof {
                                swap_id,
                                proof: local_params.proof.clone().expect("local always some"),
                            },
                        }))
                    } else {
                        Ok(None)
                    }
                    // nothing to do yet, waiting for Msg
                    // CoreArbitratingSetup to proceed
                } else {
                    error!("{} | only Some(Wallet::Alice)", swap_id.swap_id(),);
                    Err(Error::Farcaster("Needs to be an Alice wallet".to_string()))
                }
            }
            _ => Err(Error::Farcaster(
                "Handle bob reveal can only handle a Bob Reveal message".to_string(),
            )),
        }
    }

    pub fn handle_alice_reveals(
        &mut self,
        reveal: Reveal,
        swap_id: SwapId,
    ) -> Result<Option<Reveal>, Error> {
        match reveal {
            // getting parameters from counterparty Alice routed through
            // swapd, thus I'm Bob on this swap: Bob can proceed
            Reveal::Alice { parameters, proof } => {
                if let Wallet::Bob(BobState {
                    local_params,
                    key_manager,
                    deal,
                    remote_commit: Some(remote_commit),
                    remote_params, // None
                    remote_proof,  // None
                    ..
                }) = self
                {
                    // set wallet params
                    if remote_params.is_some() || remote_proof.is_some() {
                        error!("{} | Alice params or proof already set", swap_id.swap_id(),);
                        return Err(Error::Farcaster(
                            "Alice params or proof already set".to_string(),
                        ));
                    }

                    trace!("Setting Alice params: {}", parameters);
                    trace!("Setting Alice proof: {}", proof);
                    remote_commit.verify_with_reveal(&CommitmentEngine, parameters.clone())?;
                    let remote_params_candidate: Parameters = parameters.into_parameters();
                    let proof_verification = key_manager.verify_proof(
                        &remote_params_candidate.spend,
                        &remote_params_candidate.adaptor,
                        proof.proof.clone(),
                    );

                    if proof_verification.is_err() {
                        error!("{} | DLEQ proof invalid", swap_id.swap_id());
                        return Err(Error::Farcaster("DLEQ proof invalid".to_string()));
                    }
                    info!("{} | Proof successfully verified", swap_id.swap_id());
                    *remote_params = Some(remote_params_candidate);
                    *remote_proof = Some(proof.proof);

                    // if we're maker, send Reveal back to counterparty
                    let reveal = if deal.swap_role(&TradeRole::Maker) == SwapRole::Bob {
                        Some(Reveal::Bob {
                            parameters: local_params.clone().reveal_bob(swap_id),
                            proof: RevealProof {
                                swap_id,
                                proof: local_params.proof.clone().expect("local always some"),
                            },
                        })
                    } else {
                        None
                    };

                    Ok(reveal)
                } else {
                    error!("{} | only Some(Wallet::Bob)", swap_id.swap_id());
                    Err(Error::Farcaster("Only wallet::Bob is allowed".to_string()))
                }
            }
            _ => Err(Error::Farcaster(
                "handle_alice_reveals can only handle an Alice Reveal".to_string(),
            )),
        }
    }

    pub fn create_core_arb(&mut self, swap_id: SwapId) -> Result<CoreArbitratingSetup, Error> {
        if let Wallet::Bob(BobState {
            bob,
            local_params,
            key_manager,
            deal,
            funding_tx,
            remote_params,
            core_arb_setup, // None
            ..
        }) = self
        {
            // set wallet core_arb_txs
            if core_arb_setup.is_some() {
                error!("{} | Core Arb Txs already set", swap_id.swap_id(),);
                return Err(Error::Farcaster("Core arb txs already set".to_string()));
            }
            if !funding_tx.was_seen() {
                error!("{} | Funding not yet seen", swap_id.swap_id());
                return Err(Error::Farcaster("Funding not seen yet".to_string()));
            }
            let core_arbitrating_txs = bob.core_arbitrating_transactions(
                &remote_params.clone().expect("alice_params set above"),
                local_params,
                funding_tx.clone(),
                deal.to_arbitrating_params(),
            )?;
            let cosign_arbitrating_cancel =
                bob.cosign_arbitrating_cancel(key_manager, &core_arbitrating_txs)?;
            *core_arb_setup = Some(
                core_arbitrating_txs.into_arbitrating_setup(swap_id, cosign_arbitrating_cancel),
            );
            Ok(core_arb_setup
                .clone()
                .expect("This is safe, since we just set it to some"))
        } else {
            error!("{} | only Some(Wallet::Bob)", swap_id.swap_id());
            Err(Error::Farcaster("Only Wallet::Bob is allowed".to_string()))
        }
    }

    pub fn handle_refund_procedure_signatures(
        &mut self,
        refund_procedure_signatures: RefundProcedureSignatures,
        swap_id: SwapId,
    ) -> Result<HandleRefundProcedureSignaturesRes, Error> {
        let RefundProcedureSignatures {
            cancel_sig: alice_cancel_sig,
            refund_adaptor_sig,
            ..
        } = refund_procedure_signatures;
        if let Wallet::Bob(BobState {
            bob,
            local_params,
            key_manager,
            deal,
            remote_params: Some(remote_params),
            core_arb_setup: Some(core_arb_setup),
            adaptor_buy, // None
            ..
        }) = self
        {
            let core_arb_txs = core_arb_setup.clone().into_arbitrating_tx();

            bob.validate_adaptor_refund(
                key_manager,
                remote_params,
                local_params,
                &core_arb_txs,
                &refund_adaptor_sig,
            )?;

            if adaptor_buy.is_some() {
                error!("{} | adaptor_buy already set", swap_id.swap_id());
                return Err(Error::Farcaster("adaptor buy already set".to_string()));
            }
            *adaptor_buy = Some(bob.sign_adaptor_buy(
                swap_id,
                key_manager,
                remote_params,
                local_params,
                &core_arb_txs,
                deal.to_arbitrating_params(),
            )?);

            // lock
            let sig = bob.sign_arbitrating_lock(key_manager, &core_arb_txs)?;
            let tx = core_arb_setup.lock.clone();
            let mut lock_tx = LockTx::from_partial(tx);
            let lock_pubkey = key_manager.get_pubkey(ArbitratingKeyId::Lock)?;
            lock_tx.add_witness(lock_pubkey, sig)?;
            let finalized_lock_tx =
                Broadcastable::<bitcoin::Transaction>::finalize_and_extract(&mut lock_tx)?;

            // cancel
            let tx = core_arb_setup.cancel.clone();
            let mut cancel_tx = CancelTx::from_partial(tx);
            cancel_tx.add_witness(remote_params.cancel, alice_cancel_sig)?;
            cancel_tx.add_witness(local_params.cancel, core_arb_setup.cancel_sig)?;
            let finalized_cancel_tx =
                Broadcastable::<bitcoin::Transaction>::finalize_and_extract(&mut cancel_tx)?;

            // refund
            let TxSignatures { sig, adapted_sig } =
                bob.fully_sign_refund(key_manager, &core_arb_txs, &refund_adaptor_sig)?;
            let tx = core_arb_setup.refund.clone();
            let mut refund_tx = RefundTx::from_partial(tx);
            refund_tx.add_witness(local_params.refund, sig)?;
            refund_tx.add_witness(remote_params.refund, adapted_sig)?;
            let finalized_refund_tx =
                Broadcastable::<bitcoin::Transaction>::finalize_and_extract(&mut refund_tx)?;

            Ok(HandleRefundProcedureSignaturesRes {
                buy_procedure_signature: adaptor_buy
                    .clone()
                    .expect("Value was to Some in the same function"),
                lock_tx: finalized_lock_tx,
                cancel_tx: finalized_cancel_tx,
                refund_tx: finalized_refund_tx,
            })
        } else {
            error!("{} | Unknown wallet and swap id", swap_id.swap_id(),);
            Err(Error::Farcaster("Unknown wallet and swap id".to_string()))
        }
    }

    pub fn handle_core_arbitrating_setup(
        &mut self,
        core_arbitrating_setup: CoreArbitratingSetup,
        swap_id: SwapId,
    ) -> Result<HandleCoreArbitratingSetupRes, Error> {
        if let Wallet::Alice(AliceState {
            alice,
            local_params,
            key_manager,
            deal,
            remote_params: Some(bob_parameters),
            core_arb_setup,         // None
            alice_cancel_signature, // None
            adaptor_refund,         // None
            ..
        }) = self
        {
            if core_arb_setup.is_some() {
                error!("{} | core_arb_txs already set for alice", swap_id.swap_id(),);
                return Err(Error::Farcaster("Core arb already set".to_string()));
            }
            if alice_cancel_signature.is_some() {
                error!(
                    "{} | alice_cancel_sig already set for alice",
                    swap_id.swap_id(),
                );
                return Err(Error::Farcaster("Alice cancel sig already set".to_string()));
            }
            *core_arb_setup = Some(core_arbitrating_setup.clone());
            let core_arb_txs = core_arbitrating_setup.into_arbitrating_tx();
            let signed_adaptor_refund = alice.sign_adaptor_refund(
                key_manager,
                local_params,
                bob_parameters,
                &core_arb_txs,
                deal.to_arbitrating_params(),
            )?;
            *adaptor_refund = Some(signed_adaptor_refund.clone());
            let cosigned_arb_cancel = alice.cosign_arbitrating_cancel(
                key_manager,
                local_params,
                bob_parameters,
                &core_arb_txs,
                deal.to_arbitrating_params(),
            )?;
            let refund_proc_signatures = RefundProcedureSignatures {
                swap_id,
                cancel_sig: cosigned_arb_cancel,
                refund_adaptor_sig: signed_adaptor_refund,
            };
            *alice_cancel_signature = Some(refund_proc_signatures.cancel_sig);

            // cancel
            let partial_cancel_tx = core_arb_setup.as_ref().unwrap().cancel.clone();
            let mut cancel_tx = CancelTx::from_partial(partial_cancel_tx);
            cancel_tx.add_witness(local_params.cancel, alice_cancel_signature.unwrap())?;
            cancel_tx.add_witness(
                bob_parameters.cancel,
                core_arb_setup.as_ref().unwrap().cancel_sig,
            )?;
            let finalized_cancel_tx =
                Broadcastable::<bitcoin::Transaction>::finalize_and_extract(&mut cancel_tx)?;

            // punish
            let FullySignedPunish { punish, punish_sig } = alice.fully_sign_punish(
                key_manager,
                local_params,
                bob_parameters,
                &core_arb_txs,
                deal.to_arbitrating_params(),
            )?;
            let mut punish_tx = PunishTx::from_partial(punish);
            punish_tx.add_witness(
                local_params.punish.expect("Alice has punish key"),
                punish_sig,
            )?;
            let finalized_punish_tx =
                Broadcastable::<bitcoin::Transaction>::finalize_and_extract(&mut punish_tx)?;

            Ok(HandleCoreArbitratingSetupRes {
                refund_procedure_signatures: refund_proc_signatures,
                cancel_tx: finalized_cancel_tx,
                punish_tx: finalized_punish_tx,
            })
        } else {
            error!("{} | only Wallet::Alice", swap_id.swap_id(),);
            Err(Error::Farcaster(
                "Only Wallet::Alice is allowed".to_string(),
            ))
        }
    }

    pub fn handle_buy_procedure_signature(
        &mut self,
        buy_procedure_signature: BuyProcedureSignature,
        swap_id: SwapId,
    ) -> Result<HandleBuyProcedureSignatureRes, Error> {
        trace!("wallet received buyproceduresignature");
        if let Wallet::Alice(AliceState {
            alice,
            local_params: alice_params,
            key_manager,
            deal,
            remote_params: Some(bob_parameters),
            core_arb_setup: Some(core_arb_setup),
            alice_cancel_signature: Some(alice_cancel_sig),
            ..
        }) = self
        {
            let core_arb_txs = core_arb_setup.clone().into_arbitrating_tx();

            // cancel
            let tx = core_arb_setup.cancel.clone();
            let mut cancel_tx = CancelTx::from_partial(tx);
            cancel_tx.add_witness(alice_params.cancel, *alice_cancel_sig)?;
            cancel_tx.add_witness(bob_parameters.cancel, core_arb_setup.cancel_sig)?;
            let finalized_cancel_tx =
                Broadcastable::<bitcoin::Transaction>::finalize_and_extract(&mut cancel_tx)?;

            // buy
            let mut buy_tx = BuyTx::from_partial(buy_procedure_signature.buy.clone());
            alice.validate_adaptor_buy(
                key_manager,
                alice_params,
                bob_parameters,
                &core_arb_txs,
                deal.to_arbitrating_params(),
                &buy_procedure_signature,
            )?;
            let TxSignatures { sig, adapted_sig } = alice.fully_sign_buy(
                key_manager,
                alice_params,
                bob_parameters,
                &core_arb_txs,
                deal.to_arbitrating_params(),
                &buy_procedure_signature,
            )?;
            buy_tx.add_witness(key_manager.get_pubkey(ArbitratingKeyId::Buy)?, sig)?;
            buy_tx.add_witness(bob_parameters.buy, adapted_sig)?;
            let finalized_buy_tx =
                Broadcastable::<bitcoin::Transaction>::finalize_and_extract(&mut buy_tx)?;
            trace!("wallet sends fullysignedbuy");
            Ok(HandleBuyProcedureSignatureRes {
                cancel_tx: finalized_cancel_tx,
                buy_tx: finalized_buy_tx,
            })
            // buy_adaptor_sig
        } else {
            error!("{} | could not get alice's wallet", swap_id.swap_id());
            Err(Error::Farcaster("Could not get alice wallet".to_string()))
        }
    }
}

pub fn create_funding(
    key_manager: &mut KeyManager,
    net: farcaster_core::blockchain::Network,
) -> Result<FundingTx, Error> {
    let pk = key_manager.get_pubkey(ArbitratingKeyId::Lock)?;
    Ok(FundingTx::initialize(pk, net)?)
}

pub fn funding_update(funding: &mut FundingTx, tx: bitcoin::Transaction) -> Result<(), Error> {
    funding.update(tx).map_err(Into::into)
}
