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
    crypto::{ArbitratingKeyId, CommitmentEngine, GenerateKey, ProveCrossGroupDleq, SharedKeyId},
    impl_strict_encoding,
    monero::{Monero, SHARED_VIEW_KEY_ID},
    role::{SwapRole, TradeRole},
    swap::btcxmr::{
        message::{
            BuyProcedureSignature, CommitAliceParameters, CommitBobParameters, CoreArbitratingSetup,
        },
        FullySignedPunish, TxSignatures,
    },
    swap::btcxmr::{
        message::{
            RefundProcedureSignatures, RevealAliceParameters, RevealBobParameters, RevealProof,
        },
        Alice, Bob, Deal, EncryptedSignature, KeyManager, Parameters,
    },
    transaction::{Broadcastable, Fundable, Transaction, TxLabel, Witnessable},
};
use strict_encoding::{StrictDecode, StrictEncode};

use crate::{
    bus::{
        ctl::{CtlMsg, Tx},
        p2p::Reveal,
        AddressSecretKey, BitcoinSecretKeyInfo, MoneroSecretKeyInfo,
    },
    event::Event,
    service::Reporter,
    syncerd::{SweepBitcoinAddress, SweepMoneroAddress},
    Error, LogStyle, ServiceId,
};

use super::runtime::{Runtime, SwapLogging};

pub struct HandleRefundProcedureSignaturesRes {
    pub buy_procedure_signature: BuyProcedureSignature,
    pub lock_tx: bitcoin::Transaction,
    pub bob_txs: BobTxs,
}

pub struct HandleCoreArbitratingSetupRes {
    pub refund_procedure_signatures: RefundProcedureSignatures,
    pub alice_txs: AliceTxs,
    pub alice_cancel_signature: Signature,
    pub adaptor_refund: WrappedEncryptedSignature,
}

pub struct HandleBuyProcedureSignatureRes {
    pub cancel_tx: bitcoin::Transaction,
    pub buy_tx: bitcoin::Transaction,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct BobTxs {
    pub cancel_tx: bitcoin::Transaction,
    pub refund_tx: bitcoin::Transaction,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct AliceTxs {
    pub cancel_tx: bitcoin::Transaction,
    pub punish_tx: bitcoin::Transaction,
}

#[derive(Display, Clone, Debug)]
#[display("Encrypted Signature")]
pub struct WrappedEncryptedSignature(EncryptedSignature);

impl Encodable for WrappedEncryptedSignature {
    fn consensus_encode<W: std::io::Write>(&self, writer: &mut W) -> Result<usize, std::io::Error> {
        self.0.consensus_encode(writer)
    }
}
impl Decodable for WrappedEncryptedSignature {
    fn consensus_decode<D: std::io::Read>(d: &mut D) -> Result<Self, consensus::Error> {
        Ok(WrappedEncryptedSignature(Decodable::consensus_decode(d)?))
    }
}
impl_strict_encoding!(WrappedEncryptedSignature);

#[derive(Display, Clone, Debug, StrictEncode, StrictDecode)]
#[display("Alice's Swap Key Manager")]
pub struct AliceSwapKeyManager {
    pub alice: Alice,
    pub local_params: Parameters,
    pub key_manager: KeyManager,
    pub target_bitcoin_address: bitcoin::Address,
    pub target_monero_address: monero::Address,
}

impl AliceSwapKeyManager {
    pub fn new(
        alice: Alice,
        local_params: Parameters,
        key_manager: KeyManager,
        target_bitcoin_address: bitcoin::Address,
        target_monero_address: monero::Address,
    ) -> Self {
        Self {
            alice,
            local_params,
            key_manager,
            target_bitcoin_address,
            target_monero_address,
        }
    }
}

#[derive(Display, Clone, Debug)]
#[display("Bob's Swap Key Manager")]
pub struct BobSwapKeyManager {
    pub bob: Bob,
    pub local_params: Parameters,
    pub key_manager: KeyManager,
    pub funding_tx: FundingTx,
    pub target_bitcoin_address: bitcoin::Address,
    pub target_monero_address: monero::Address,
}

impl Encodable for BobSwapKeyManager {
    fn consensus_encode<W: io::Write>(&self, writer: &mut W) -> Result<usize, io::Error> {
        let mut len = self.bob.consensus_encode(writer)?;
        len += self.local_params.consensus_encode(writer)?;
        len += self.key_manager.consensus_encode(writer)?;
        len += self.funding_tx.consensus_encode(writer)?;
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

impl Decodable for BobSwapKeyManager {
    fn consensus_decode<D: io::Read>(d: &mut D) -> Result<Self, consensus::Error> {
        Ok(BobSwapKeyManager {
            bob: Decodable::consensus_decode(d)?,
            local_params: Decodable::consensus_decode(d)?,
            key_manager: Decodable::consensus_decode(d)?,
            funding_tx: Decodable::consensus_decode(d)?,
            target_bitcoin_address: bitcoin::Address::from_canonical_bytes(
                farcaster_core::unwrap_vec_ref!(d).as_ref(),
            )?,
            target_monero_address: monero::Address::from_canonical_bytes(
                farcaster_core::unwrap_vec_ref!(d).as_ref(),
            )?,
        })
    }
}

impl BobSwapKeyManager {
    pub fn new(
        bob: Bob,
        local_params: Parameters,
        key_manager: KeyManager,
        funding_tx: FundingTx,
        target_bitcoin_address: bitcoin::Address,
        target_monero_address: monero::Address,
    ) -> Self {
        Self {
            bob,
            local_params,
            key_manager,
            funding_tx,
            target_bitcoin_address,
            target_monero_address,
        }
    }
}

impl_strict_encoding!(BobSwapKeyManager);

impl AliceSwapKeyManager {
    pub fn local_params(&self) -> Parameters {
        self.local_params.clone()
    }

    pub fn new_taker_alice(
        runtime: &mut Runtime,
        target_bitcoin_address: bitcoin::Address,
        target_monero_address: monero::Address,
        mut key_manager: KeyManager,
    ) -> Result<Self, Error> {
        // since we're takers, we are on the other side of the trade
        let alice = Alice::new(
            BitcoinSegwitV0::new(),
            Monero,
            target_bitcoin_address.clone(),
            FeePriority::Low,
        );
        let local_params = alice.generate_parameters(&mut key_manager, &runtime.deal)?;
        Ok(AliceSwapKeyManager::new(
            alice,
            local_params,
            key_manager,
            // None,
            target_bitcoin_address,
            target_monero_address,
        ))
    }

    pub fn taker_commit(
        &self,
        event: &mut Event,
        runtime: &mut Runtime,
    ) -> Result<CommitAliceParameters, Error> {
        let AliceSwapKeyManager { local_params, .. } = self;
        runtime.log_info(format!(
            "{} to Maker remote peer",
            "Proposing to take swap".bright_white_bold(),
        ));

        let msg = format!(
            "Proposing to take swap {} to Maker remote peer",
            runtime.swap_id.swap_id()
        );
        // Ignoring possible reporting errors here and after: do not want to
        // halt the swap just because the client disconnected
        let _ = runtime.report_progress_message(event.endpoints, msg);

        let engine = CommitmentEngine;
        Ok(local_params.commit_alice(runtime.swap_id, &engine))
    }

    pub fn new_maker_alice(
        runtime: &mut Runtime,
        target_bitcoin_address: bitcoin::Address,
        target_monero_address: monero::Address,
        mut key_manager: KeyManager,
    ) -> Result<Self, Error> {
        let alice = Alice::new(
            BitcoinSegwitV0::new(),
            Monero,
            target_bitcoin_address.clone(),
            FeePriority::Low,
        );
        let local_params = alice.generate_parameters(&mut key_manager, &runtime.deal)?;
        runtime.log_info(format!("Loading {}", "Alice Swap Key Manager".label()));
        Ok(AliceSwapKeyManager::new(
            alice,
            local_params,
            key_manager,
            target_bitcoin_address,
            target_monero_address,
        ))
    }

    pub fn maker_commit(
        &self,
        event: &mut Event,
        runtime: &mut Runtime,
    ) -> Result<CommitAliceParameters, Error> {
        let AliceSwapKeyManager { local_params, .. } = self;
        runtime.log_info(format!(
            "{} as Maker from Taker through peerd {}",
            "Accepting swap".bright_white_bold(),
            runtime.peer_service.bright_blue_italic()
        ));

        let msg = format!(
            "Accepting swap {} as Maker from Taker through peerd {}",
            runtime.swap_id, runtime.peer_service
        );
        // Ignoring possible reporting errors here and after: do not want to
        // halt the swap just because the enquirer (farcasterd) disconnected
        let _ = runtime.report_progress_message(event.endpoints, msg);

        let engine = CommitmentEngine;
        Ok(local_params.commit_alice(runtime.swap_id, &engine))
    }

    pub fn handle_bob_reveals(
        &mut self,
        runtime: &mut Runtime,
        parameters: RevealBobParameters,
        proof: RevealProof,
        remote_commit: CommitBobParameters,
    ) -> Result<(Option<Reveal>, Parameters), Error> {
        let AliceSwapKeyManager {
            local_params,
            key_manager,
            ..
        } = self;
        runtime.log_trace(format!("Verifying with Bob params: {}", parameters));
        runtime.log_trace(format!("Verifying with Bob proof: {}", proof));
        remote_commit.verify_with_reveal(&CommitmentEngine, parameters.clone())?;
        let remote_params_candidate: Parameters = parameters.into_parameters();
        let proof_verification = key_manager.verify_proof(
            &remote_params_candidate.spend,
            &remote_params_candidate.adaptor,
            proof.proof,
        );
        if let Err(err) = proof_verification {
            let msg = format!("DLEQ proof from Bob is invalid: {}.", err);
            runtime.log_error(&msg);
            return Err(Error::Farcaster(msg));
        }
        runtime.log_info("DLEQ proof from Bob successfully verified.");

        // if we're maker, send Reveal back to counterparty
        if runtime.deal.swap_role(&TradeRole::Maker) == SwapRole::Alice {
            Ok((
                Some(Reveal::Alice {
                    parameters: local_params.clone().reveal_alice(runtime.swap_id),
                    proof: RevealProof {
                        swap_id: runtime.swap_id,
                        proof: local_params.proof.clone().expect("local always some"),
                    },
                }),
                remote_params_candidate,
            ))
        } else {
            Ok((None, remote_params_candidate))
        }
        // Nothing to do yet, waiting for Msg CoreArbitratingSetup to proceed
    }

    pub fn create_reveal_from_local_params(&self, runtime: &mut Runtime) -> Result<Reveal, Error> {
        let AliceSwapKeyManager { local_params, .. } = self;
        runtime.log_debug("Generating reveal alice message");
        Ok(Reveal::Alice {
            parameters: local_params.clone().reveal_alice(runtime.swap_id),
            proof: RevealProof {
                swap_id: runtime.swap_id,
                proof: local_params.proof.clone().expect("local always some"),
            },
        })
    }

    pub fn process_refund_tx(
        &mut self,
        event: &mut Event,
        runtime: &mut Runtime,
        refund_tx: bitcoin::Transaction,
        bob_params: Parameters,
        adaptor_refund: WrappedEncryptedSignature,
        acc_lock_height_lower_bound: u64,
    ) -> Result<SweepMoneroAddress, Error> {
        let AliceSwapKeyManager {
            alice,
            local_params,
            key_manager,
            target_monero_address,
            ..
        } = self;

        let sk_b_btc =
            alice.recover_accordant_key(key_manager, &bob_params, adaptor_refund.0, refund_tx);
        let mut sk_b_btc_buf: Vec<u8> = (*sk_b_btc.as_ref()).into();
        sk_b_btc_buf.reverse();
        let sk_b = monero::PrivateKey::from_slice(sk_b_btc_buf.as_ref())
            .expect("Valid Monero Private Key");

        runtime.log_info(format!(
            "Extracted monero key from Refund tx: {}",
            sk_b.label()
        ));

        let sk_a = key_manager.get_or_derive_monero_spend_key()?;
        let spend = sk_a + sk_b;
        runtime.log_info(format!(
            "Full secret monero spending key: {}",
            spend.bright_green_bold()
        ));

        let view_key_bob = *bob_params
            .accordant_shared_keys
            .clone()
            .into_iter()
            .find(|vk| vk.tag() == &SharedKeyId::new(SHARED_VIEW_KEY_ID))
            .expect("We always expect to find this tag")
            .elem();

        let view_key_alice = *local_params
            .accordant_shared_keys
            .clone()
            .into_iter()
            .find(|vk| vk.tag() == &SharedKeyId::new(SHARED_VIEW_KEY_ID))
            .expect("We always expect to find this tag")
            .elem();
        let view = view_key_alice + view_key_bob;
        runtime.log_info(format!(
            "Full secret monero view key: {}",
            view.bright_green_bold()
        ));
        let network = runtime.deal.parameters.network.into();
        let keypair = monero::KeyPair { view, spend };
        let corresponding_address = monero::Address::from_keypair(network, &keypair);
        runtime.log_info(format!(
            "Corresponding address: {}",
            corresponding_address.addr()
        ));

        event.send_ctl_service(
            ServiceId::Database,
            CtlMsg::SetAddressSecretKey(AddressSecretKey::Monero {
                address: corresponding_address,
                secret_key_info: MoneroSecretKeyInfo {
                    swap_id: Some(runtime.swap_id),
                    view: keypair.view.as_bytes().try_into().unwrap(),
                    spend: keypair.spend.as_bytes().try_into().unwrap(),
                    creation_height: acc_lock_height_lower_bound,
                },
            }),
        )?;

        Ok(SweepMoneroAddress {
            source_view_key: view,
            source_spend_key: spend,
            destination_address: *target_monero_address,
            minimum_balance: runtime.deal.parameters.accordant_amount,
            from_height: Some(acc_lock_height_lower_bound),
        })
    }

    pub fn handle_core_arbitrating_setup(
        &mut self,
        runtime: &mut Runtime,
        core_arbitrating_setup: CoreArbitratingSetup,
        bob_parameters: &Parameters,
    ) -> Result<HandleCoreArbitratingSetupRes, Error> {
        let AliceSwapKeyManager {
            alice,
            local_params,
            key_manager,
            ..
        } = self;
        let core_arbitrating_txs = core_arbitrating_setup.clone().into_arbitrating_tx();
        let signed_adaptor_refund = alice.sign_adaptor_refund(
            key_manager,
            local_params,
            bob_parameters,
            &core_arbitrating_txs,
            runtime.deal.to_arbitrating_params(),
        )?;
        let adaptor_refund = WrappedEncryptedSignature(signed_adaptor_refund.clone());
        let cosigned_arb_cancel = alice.cosign_arbitrating_cancel(
            key_manager,
            local_params,
            bob_parameters,
            &core_arbitrating_txs,
            runtime.deal.to_arbitrating_params(),
        )?;
        let refund_proc_signatures = RefundProcedureSignatures {
            swap_id: runtime.swap_id,
            cancel_sig: cosigned_arb_cancel,
            refund_adaptor_sig: signed_adaptor_refund,
        };
        let alice_cancel_signature = refund_proc_signatures.cancel_sig;

        // cancel
        let mut cancel_tx = CancelTx::from_partial(core_arbitrating_setup.cancel);
        cancel_tx.add_witness(local_params.cancel, alice_cancel_signature)?;
        cancel_tx.add_witness(bob_parameters.cancel, core_arbitrating_setup.cancel_sig)?;
        let finalized_cancel_tx =
            Broadcastable::<bitcoin::Transaction>::finalize_and_extract(&mut cancel_tx)?;

        // punish
        let FullySignedPunish { punish, punish_sig } = alice.fully_sign_punish(
            key_manager,
            local_params,
            bob_parameters,
            &core_arbitrating_txs,
            runtime.deal.to_arbitrating_params(),
        )?;
        let mut punish_tx = PunishTx::from_partial(punish);
        punish_tx.add_witness(
            local_params.punish.expect("Alice has punish key"),
            punish_sig,
        )?;
        let finalized_punish_tx =
            Broadcastable::<bitcoin::Transaction>::finalize_and_extract(&mut punish_tx)?;
        runtime.log_info(format!("{} transaction created", TxLabel::Cancel));
        runtime.log_info(format!("{} transaction created", TxLabel::Punish));

        Ok(HandleCoreArbitratingSetupRes {
            refund_procedure_signatures: refund_proc_signatures,
            alice_txs: AliceTxs {
                cancel_tx: finalized_cancel_tx,
                punish_tx: finalized_punish_tx,
            },
            alice_cancel_signature,
            adaptor_refund,
        })
    }

    pub fn handle_buy_procedure_signature(
        &mut self,
        runtime: &mut Runtime,
        buy_procedure_signature: BuyProcedureSignature,
        bob_parameters: &Parameters,
        core_arbitrating_setup: CoreArbitratingSetup,
        alice_cancel_signature: Signature,
    ) -> Result<HandleBuyProcedureSignatureRes, Error> {
        runtime.log_trace("Swap key manager handling buy procedure signature.");
        let AliceSwapKeyManager {
            alice,
            local_params: alice_params,
            key_manager,
            ..
        } = self;
        let core_arbitrating_txs = core_arbitrating_setup.clone().into_arbitrating_tx();

        // cancel
        let mut cancel_tx = CancelTx::from_partial(core_arbitrating_setup.cancel);
        cancel_tx.add_witness(alice_params.cancel, alice_cancel_signature)?;
        cancel_tx.add_witness(bob_parameters.cancel, core_arbitrating_setup.cancel_sig)?;
        let finalized_cancel_tx =
            Broadcastable::<bitcoin::Transaction>::finalize_and_extract(&mut cancel_tx)?;

        // buy
        let mut buy_tx = BuyTx::from_partial(buy_procedure_signature.buy.clone());
        alice.validate_adaptor_buy(
            key_manager,
            alice_params,
            bob_parameters,
            &core_arbitrating_txs,
            runtime.deal.to_arbitrating_params(),
            &buy_procedure_signature,
        )?;
        let TxSignatures { sig, adapted_sig } = alice.fully_sign_buy(
            key_manager,
            alice_params,
            bob_parameters,
            &core_arbitrating_txs,
            runtime.deal.to_arbitrating_params(),
            &buy_procedure_signature,
        )?;
        buy_tx.add_witness(key_manager.get_pubkey(ArbitratingKeyId::Buy)?, sig)?;
        buy_tx.add_witness(bob_parameters.buy, adapted_sig)?;
        let finalized_buy_tx =
            Broadcastable::<bitcoin::Transaction>::finalize_and_extract(&mut buy_tx)?;
        runtime.log_trace("Swap key manager returning fully signed buy");
        Ok(HandleBuyProcedureSignatureRes {
            cancel_tx: finalized_cancel_tx,
            buy_tx: finalized_buy_tx,
        })
        // buy_adaptor_sig
    }
}

impl BobSwapKeyManager {
    pub fn local_params(&self) -> Parameters {
        self.local_params.clone()
    }

    pub fn process_funding_tx(&mut self, runtime: &mut Runtime, tx: Tx) -> Result<(), Error> {
        if let Tx::Funding(tx) = tx {
            if self.funding_tx.was_seen() {
                runtime.log_warn("Funding was previously updated, ignoring");
                return Err(Error::Farcaster("Funding already updated".to_string()));
            }
            self.funding_tx.update(tx)?;
            runtime.log_debug(
                "Bob's swap key manager informs swapd that funding was successfully updated",
            );
            Ok(())
        } else {
            Err(Error::Farcaster(
                "Process funding tx only processes funding transactions".to_string(),
            ))
        }
    }

    pub fn funding_address(&self) -> Option<bitcoin::Address> {
        self.funding_tx.get_address().ok()
    }

    pub fn process_get_sweep_bitcoin_address(
        &mut self,
        source_address: bitcoin::Address,
    ) -> Result<SweepBitcoinAddress, Error> {
        let BobSwapKeyManager {
            key_manager, bob, ..
        } = self;
        let source_secret_key = key_manager.get_or_derive_bitcoin_key(ArbitratingKeyId::Lock)?;
        let destination_address = bob.refund_address.clone();
        Ok(SweepBitcoinAddress {
            source_secret_key,
            source_address,
            destination_address,
        })
    }

    pub fn new_taker_bob(
        event: &mut Event,
        runtime: &mut Runtime,
        target_bitcoin_address: bitcoin::Address,
        target_monero_address: monero::Address,
        mut key_manager: KeyManager,
    ) -> Result<Self, Error> {
        let Deal {
            parameters: deal_parameters,
            ..
        } = runtime.deal.clone();

        let bob = Bob::new(
            BitcoinSegwitV0::new(),
            Monero,
            target_bitcoin_address.clone(),
            FeePriority::Low,
        );
        let local_params = bob.generate_parameters(&mut key_manager, &runtime.deal)?;
        let funding = create_funding(&mut key_manager, deal_parameters.network)?;
        let funding_addr = funding.get_address()?;
        event.send_ctl_service(
            ServiceId::Database,
            CtlMsg::SetAddressSecretKey(AddressSecretKey::Bitcoin {
                address: funding_addr,
                secret_key_info: BitcoinSecretKeyInfo {
                    swap_id: Some(runtime.swap_id),
                    secret_key: key_manager.get_or_derive_bitcoin_key(ArbitratingKeyId::Lock)?,
                },
            }),
        )?;
        runtime.log_info(format!("Loading {}", "Bob's Swap Key Manager".label()));
        Ok(BobSwapKeyManager::new(
            bob,
            local_params,
            key_manager,
            funding,
            target_bitcoin_address,
            target_monero_address,
        ))
    }

    pub fn taker_commit(
        &self,
        event: &mut Event,
        runtime: &mut Runtime,
    ) -> Result<CommitBobParameters, Error> {
        let BobSwapKeyManager { local_params, .. } = self;
        runtime.log_info(format!(
            "{} to Maker remote peer",
            "Proposing to take swap".bright_white_bold(),
        ));

        let msg = format!(
            "Proposing to take swap {} to Maker remote peer",
            runtime.swap_id.swap_id()
        );
        // Ignoring possible reporting errors here and after: do not want to
        // halt the swap just because the client disconnected
        let _ = runtime.report_progress_message(event.endpoints, msg);

        let engine = CommitmentEngine;
        Ok(local_params.commit_bob(runtime.swap_id, &engine))
    }

    pub fn new_maker_bob(
        event: &mut Event,
        runtime: &mut Runtime,
        target_bitcoin_address: bitcoin::Address,
        target_monero_address: monero::Address,
        mut key_manager: KeyManager,
    ) -> Result<Self, Error> {
        let Deal {
            parameters: deal_parameters,
            ..
        } = runtime.deal.clone();
        let bob = Bob::new(
            BitcoinSegwitV0::new(),
            Monero,
            target_bitcoin_address.clone(),
            FeePriority::Low,
        );
        let local_params = bob.generate_parameters(&mut key_manager, &runtime.deal)?;
        let funding = create_funding(&mut key_manager, deal_parameters.network)?;
        let funding_addr = funding.get_address()?;
        event.send_ctl_service(
            ServiceId::Database,
            CtlMsg::SetAddressSecretKey(AddressSecretKey::Bitcoin {
                address: funding_addr,
                secret_key_info: BitcoinSecretKeyInfo {
                    swap_id: Some(runtime.swap_id),
                    secret_key: key_manager.get_or_derive_bitcoin_key(ArbitratingKeyId::Lock)?,
                },
            }),
        )?;
        runtime.log_info(format!("Loading {}", "Bob's Swap Key Manager".label()));
        Ok(BobSwapKeyManager::new(
            bob,
            local_params,
            key_manager,
            funding,
            target_bitcoin_address,
            target_monero_address,
        ))
    }

    pub fn maker_commit(
        &self,
        event: &mut Event,
        runtime: &mut Runtime,
    ) -> Result<CommitBobParameters, Error> {
        let BobSwapKeyManager { local_params, .. } = self;
        runtime.log_info(format!(
            "{} as Maker from Taker through peerd {}",
            "Accepting swap".bright_white_bold(),
            runtime.peer_service.bright_blue_italic()
        ));

        let msg = format!(
            "Accepting swap {} as Maker from Taker through peerd {}",
            runtime.swap_id, runtime.peer_service
        );
        // Ignoring possible reporting errors here and after: do not want to
        // halt the swap just because the enquirer (farcasterd) disconnected
        let _ = runtime.report_progress_message(event.endpoints, msg);

        let engine = CommitmentEngine;
        Ok(local_params.commit_bob(runtime.swap_id, &engine))
    }

    pub fn create_reveal_from_local_params(&self, runtime: &mut Runtime) -> Result<Reveal, Error> {
        let BobSwapKeyManager { local_params, .. } = self;
        // craft the correct reveal depending on role
        Ok(Reveal::Bob {
            parameters: local_params.clone().reveal_bob(runtime.swap_id),
            proof: RevealProof {
                swap_id: runtime.swap_id,
                proof: local_params.proof.clone().expect("local always some"),
            },
        })
    }

    pub fn process_buy_tx(
        &mut self,
        runtime: &mut Runtime,
        buy_tx: bitcoin::Transaction,
        event: &mut Event,
        alice_params: Parameters,
        adaptor_buy: BuyProcedureSignature,
        acc_lock_height_lower_bound: u64,
    ) -> Result<SweepMoneroAddress, Error> {
        let BobSwapKeyManager {
            bob,
            local_params,
            key_manager,
            target_monero_address,
            ..
        } = self;
        let sk_a_btc = bob.recover_accordant_key(
            key_manager,
            &alice_params,
            adaptor_buy.buy_adaptor_sig,
            buy_tx,
        );
        let mut sk_a_btc_buf: Vec<u8> = (*sk_a_btc.as_ref()).into();
        sk_a_btc_buf.reverse();
        let sk_a = monero::PrivateKey::from_slice(sk_a_btc_buf.as_ref())
            .expect("Valid Monero Private Key");
        runtime.log_info(format!(
            "Extracted monero key from Buy tx: {}",
            sk_a.label()
        ));
        let sk_b = key_manager.get_or_derive_monero_spend_key()?;
        let spend = sk_a + sk_b;
        runtime.log_info(format!(
            "Full secret monero spending key: {}",
            spend.bright_green_bold()
        ));
        let view_key_alice = *alice_params
            .accordant_shared_keys
            .clone()
            .into_iter()
            .find(|vk| vk.tag() == &SharedKeyId::new(SHARED_VIEW_KEY_ID))
            .expect("We always expect to find this tag")
            .elem();

        let view_key_bob = *local_params
            .accordant_shared_keys
            .clone()
            .into_iter()
            .find(|vk| vk.tag() == &SharedKeyId::new(SHARED_VIEW_KEY_ID))
            .expect("We always expect to find this tag")
            .elem();
        let view = view_key_alice + view_key_bob;
        runtime.log_info(format!(
            "Full secret monero view key: {}",
            view.bright_green_bold()
        ));
        let network = runtime.deal.parameters.network.into();
        let keypair = monero::KeyPair { view, spend };
        let corresponding_address = monero::Address::from_keypair(network, &keypair);
        runtime.log_info(format!(
            "Corresponding address: {}",
            corresponding_address.addr()
        ));

        event.send_ctl_service(
            ServiceId::Database,
            CtlMsg::SetAddressSecretKey(AddressSecretKey::Monero {
                address: corresponding_address,
                secret_key_info: MoneroSecretKeyInfo {
                    swap_id: Some(runtime.swap_id),
                    view: keypair.view.as_bytes().try_into().unwrap(),
                    spend: keypair.spend.as_bytes().try_into().unwrap(),
                    creation_height: acc_lock_height_lower_bound,
                },
            }),
        )?;

        Ok(SweepMoneroAddress {
            source_view_key: view,
            source_spend_key: spend,
            destination_address: *target_monero_address,
            minimum_balance: runtime.deal.parameters.accordant_amount,
            from_height: Some(acc_lock_height_lower_bound),
        })
    }

    pub fn handle_alice_reveals(
        &mut self,
        runtime: &mut Runtime,
        parameters: RevealAliceParameters,
        proof: RevealProof,
        remote_commit: CommitAliceParameters,
    ) -> Result<(Option<Reveal>, Parameters), Error> {
        let BobSwapKeyManager {
            local_params,
            key_manager,
            ..
        } = self;
        runtime.log_trace(format!("Verifying with Alice params: {}", parameters));
        runtime.log_trace(format!("Verifying with Alice proof: {}", proof));
        remote_commit.verify_with_reveal(&CommitmentEngine, parameters.clone())?;
        let remote_params_candidate: Parameters = parameters.into_parameters();
        let proof_verification = key_manager.verify_proof(
            &remote_params_candidate.spend,
            &remote_params_candidate.adaptor,
            proof.proof,
        );
        if let Err(err) = proof_verification {
            let msg = format!("DLEQ proof from Alice is invalid: {}.", err);
            runtime.log_error(&msg);
            return Err(Error::Farcaster(msg));
        }
        runtime.log_info("DLEQ proof from Alice successfully verified.");

        // if we're maker, send Reveal back to counterparty
        let reveal = if runtime.deal.swap_role(&TradeRole::Maker) == SwapRole::Bob {
            Some(Reveal::Bob {
                parameters: local_params.clone().reveal_bob(runtime.swap_id),
                proof: RevealProof {
                    swap_id: runtime.swap_id,
                    proof: local_params.proof.clone().expect("local always some"),
                },
            })
        } else {
            None
        };

        Ok((reveal, remote_params_candidate))
    }

    pub fn create_core_arb(
        &mut self,
        runtime: &mut Runtime,
        remote_params: &Parameters,
    ) -> Result<CoreArbitratingSetup, Error> {
        let BobSwapKeyManager {
            bob,
            local_params,
            key_manager,
            funding_tx,
            ..
        } = self;
        if !funding_tx.was_seen() {
            runtime.log_error("Funding not yet seen.");
            return Err(Error::Farcaster("Funding not seen yet".to_string()));
        }
        let core_arbitrating_txs = bob.core_arbitrating_transactions(
            remote_params,
            local_params,
            funding_tx.clone(),
            runtime.deal.to_arbitrating_params(),
        )?;
        let cosign_arbitrating_cancel =
            bob.cosign_arbitrating_cancel(key_manager, &core_arbitrating_txs)?;
        Ok(core_arbitrating_txs.into_arbitrating_setup(runtime.swap_id, cosign_arbitrating_cancel))
    }

    pub fn handle_refund_procedure_signatures(
        &mut self,
        runtime: &mut Runtime,
        refund_procedure_signatures: RefundProcedureSignatures,
        remote_params: &Parameters,
        core_arbitrating_setup: CoreArbitratingSetup,
    ) -> Result<HandleRefundProcedureSignaturesRes, Error> {
        let RefundProcedureSignatures {
            cancel_sig: alice_cancel_sig,
            refund_adaptor_sig,
            ..
        } = refund_procedure_signatures;
        let BobSwapKeyManager {
            bob,
            local_params,
            key_manager,
            ..
        } = self;
        let core_arbitrating_txs = core_arbitrating_setup.clone().into_arbitrating_tx();

        bob.validate_adaptor_refund(
            key_manager,
            remote_params,
            local_params,
            &core_arbitrating_txs,
            &refund_adaptor_sig,
        )?;

        let adaptor_buy = bob.sign_adaptor_buy(
            runtime.swap_id,
            key_manager,
            remote_params,
            local_params,
            &core_arbitrating_txs,
            runtime.deal.to_arbitrating_params(),
        )?;

        // lock
        let sig = bob.sign_arbitrating_lock(key_manager, &core_arbitrating_txs)?;
        let mut lock_tx = LockTx::from_partial(core_arbitrating_setup.lock);
        let lock_pubkey = key_manager.get_pubkey(ArbitratingKeyId::Lock)?;
        lock_tx.add_witness(lock_pubkey, sig)?;
        let finalized_lock_tx =
            Broadcastable::<bitcoin::Transaction>::finalize_and_extract(&mut lock_tx)?;

        // cancel
        let mut cancel_tx = CancelTx::from_partial(core_arbitrating_setup.cancel);
        cancel_tx.add_witness(remote_params.cancel, alice_cancel_sig)?;
        cancel_tx.add_witness(local_params.cancel, core_arbitrating_setup.cancel_sig)?;
        let finalized_cancel_tx =
            Broadcastable::<bitcoin::Transaction>::finalize_and_extract(&mut cancel_tx)?;

        // refund
        let TxSignatures { sig, adapted_sig } =
            bob.fully_sign_refund(key_manager, &core_arbitrating_txs, &refund_adaptor_sig)?;
        let mut refund_tx = RefundTx::from_partial(core_arbitrating_setup.refund);
        refund_tx.add_witness(local_params.refund, sig)?;
        refund_tx.add_witness(remote_params.refund, adapted_sig)?;
        let finalized_refund_tx =
            Broadcastable::<bitcoin::Transaction>::finalize_and_extract(&mut refund_tx)?;

        runtime.log_info(format!("{} transaction created", TxLabel::Cancel));
        runtime.log_info(format!("{} transaction created", TxLabel::Refund));
        runtime.log_info(format!("{} transaction created", TxLabel::Lock));

        Ok(HandleRefundProcedureSignaturesRes {
            buy_procedure_signature: adaptor_buy,
            lock_tx: finalized_lock_tx,
            bob_txs: BobTxs {
                cancel_tx: finalized_cancel_tx,
                refund_tx: finalized_refund_tx,
            },
        })
    }
}

pub fn create_funding(
    key_manager: &mut KeyManager,
    net: farcaster_core::blockchain::Network,
) -> Result<FundingTx, Error> {
    let pk = key_manager.get_pubkey(ArbitratingKeyId::Lock)?;
    Ok(FundingTx::initialize(pk, net)?)
}
