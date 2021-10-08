use std::{
    collections::{HashMap, HashSet},
    convert::{TryFrom, TryInto},
    io::{self, Write},
    ptr::swap_nonoverlapping,
    str::FromStr,
};

use crate::rpc::{
    request::{self, Commit, Keys, Msg, Params, Reveal, Token, Tx},
    Request, ServiceBus,
};
use crate::swapd::get_swap_id;
use crate::walletd::NodeSecrets;
use crate::LogStyle;
use crate::Senders;
use crate::{Config, CtlServer, Error, Service, ServiceId};
use bitcoin::{
    hashes::hex::FromHex,
    secp256k1::{self, Signature},
    util::{
        bip32::{DerivationPath, ExtendedPrivKey},
        psbt::serialize::Deserialize,
    },
    PrivateKey, PublicKey,
};
use colored::Colorize;
use farcaster_core::{
    bitcoin::{
        segwitv0::{BuyTx, CancelTx, FundingTx, PunishTx, RefundTx},
        segwitv0::{LockTx, SegwitV0},
        Bitcoin, BitcoinSegwitV0,
    },
    blockchain::FeePriority,
    bundle::{
        AliceParameters, BobParameters, CoreArbitratingTransactions, FullySignedBuy,
        FullySignedPunish, FullySignedRefund, FundingTransaction, Proof, SignedAdaptorBuy,
        SignedAdaptorRefund, SignedArbitratingLock,
    },
    crypto::{ArbitratingKeyId, GenerateKey, SharedKeyId},
    crypto::{CommitmentEngine, ProveCrossGroupDleq},
    monero::{Monero, SHARED_VIEW_KEY_ID},
    negotiation::PublicOffer,
    protocol_message::{
        BuyProcedureSignature, CommitAliceParameters, CommitBobParameters, CoreArbitratingSetup,
        RefundProcedureSignatures,
    },
    role::{Alice, Bob, SwapRole, TradeRole},
    swap::btcxmr::{BtcXmr, KeyManager},
    swap::SwapId,
    syncer::{AddressTransaction, Boolean, Event},
    transaction::{Broadcastable, Fundable, Transaction, TxLabel, Witnessable},
};
use internet2::{LocalNode, ToNodeAddr, TypedEnum, LIGHTNING_P2P_DEFAULT_PORT};
// use lnp::{ChannelId as SwapId, TempChannelId as TempSwapId};
use microservices::esb::{self, Handler};
use request::{LaunchSwap, NodeId};

pub fn run(
    config: Config,
    wallet_token: Token,
    node_secrets: NodeSecrets,
    node_id: bitcoin::secp256k1::PublicKey,
) -> Result<(), Error> {
    let runtime = Runtime {
        identity: ServiceId::Wallet,
        wallet_token,
        node_secrets,
        node_id,
        wallets: none!(),
        swaps: none!(),
        wallet_counter: Counter(0),
    };

    Service::run(config, runtime, false)
}

pub struct Runtime {
    identity: ServiceId,
    wallet_token: Token,
    node_secrets: NodeSecrets,
    node_id: bitcoin::secp256k1::PublicKey,
    wallets: HashMap<SwapId, Wallet>,
    swaps: HashMap<SwapId, Option<Request>>,
    wallet_counter: Counter,
}

pub enum Wallet {
    Alice(
        Alice<BtcXmr>,
        AliceParameters<BtcXmr>,
        Proof<BtcXmr>,
        KeyManager,
        PublicOffer<BtcXmr>,
        Option<CommitBobParameters<BtcXmr>>,
        Option<BobParameters<BtcXmr>>,
        Option<Proof<BtcXmr>>,
        Option<CoreArbitratingSetup<BtcXmr>>,
        Option<Signature>,
        Option<SignedAdaptorRefund<farcaster_core::bitcoin::BitcoinSegwitV0>>,
    ),
    Bob(BobState),
}

struct Counter(u32);
impl Counter {
    fn increment(&mut self) -> u32 {
        self.0 += 1;
        self.0
    }
}

struct AliceWallet {
    wallet_ix: u32,
    alice: Alice<BtcXmr>,
    local_params: AliceParameters<BtcXmr>,
    local_proof: Proof<BtcXmr>,
    key_manager: KeyManager,
    pub_offer: PublicOffer<BtcXmr>,
    remote_commit: Option<CommitBobParameters<BtcXmr>>,
    remote_params: Option<BobParameters<BtcXmr>>,
    remote_proof: Option<Proof<BtcXmr>>,
    core_arb_setup: Option<CoreArbitratingSetup<BtcXmr>>,
    signature: Option<Signature>,
}

impl AliceWallet {
    fn new(
        wallet_ix: u32,
        alice: Alice<BtcXmr>,
        local_params: AliceParameters<BtcXmr>,
        local_proof: Proof<BtcXmr>,
        key_manager: KeyManager,
        pub_offer: PublicOffer<BtcXmr>,
        remote_commit: Option<CommitBobParameters<BtcXmr>>,
    ) -> Self {
        Self {
            wallet_ix,
            alice,
            local_params,
            local_proof,
            key_manager,
            pub_offer,
            remote_commit,
            remote_params: None,
            remote_proof: None,
            core_arb_setup: None,
            signature: None,
        }
    }
}

pub struct BobState {
    wallet_ix: u32,
    bob: Bob<BtcXmr>,
    local_params: BobParameters<BtcXmr>,
    local_proof: Proof<BtcXmr>,
    key_manager: KeyManager,
    pub_offer: PublicOffer<BtcXmr>,
    funding_tx: Option<FundingTx>,
    remote_commit_params: Option<CommitAliceParameters<BtcXmr>>,
    remote_params: Option<AliceParameters<BtcXmr>>,
    remote_proof: Option<Proof<BtcXmr>>,
    core_arb_setup: Option<CoreArbitratingSetup<BtcXmr>>,
    adaptor_buy: Option<SignedAdaptorBuy<Bitcoin<SegwitV0>>>,
}

impl BobState {
    fn new(
        wallet_ix: u32,
        bob: Bob<BtcXmr>,
        local_params: BobParameters<BtcXmr>,
        local_proof: Proof<BtcXmr>,
        key_manager: KeyManager,
        pub_offer: PublicOffer<BtcXmr>,
        funding_tx: Option<FundingTx>,
        remote_commit_params: Option<CommitAliceParameters<BtcXmr>>,
    ) -> Self {
        Self {
            wallet_ix,
            bob,
            local_params,
            local_proof,
            key_manager,
            pub_offer,
            funding_tx,
            remote_commit_params,
            remote_params: None,
            remote_proof: None,
            core_arb_setup: None,
            adaptor_buy: None,
        }
    }
}

impl CtlServer for Runtime {}

impl esb::Handler<ServiceBus> for Runtime {
    type Request = Request;
    type Address = ServiceId;
    type Error = Error;

    fn identity(&self) -> ServiceId {
        self.identity.clone()
    }

    fn handle(
        &mut self,
        senders: &mut esb::SenderList<ServiceBus, ServiceId>,
        bus: ServiceBus,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Self::Error> {
        match bus {
            ServiceBus::Msg => self.handle_rpc_msg(senders, source, request),
            ServiceBus::Ctl => self.handle_rpc_ctl(senders, source, request),
            _ => Err(Error::NotSupported(ServiceBus::Bridge, request.get_type())),
        }
    }

    fn handle_err(&mut self, _: esb::Error) -> Result<(), esb::Error> {
        // We do nothing and do not propagate error; it's already being reported
        // with `error!` macro by the controller. If we propagate error here
        // this will make whole daemon panic
        Ok(())
    }
}

impl Runtime {
    fn send_farcasterd(
        &self,
        senders: &mut Senders,
        message: request::Request,
    ) -> Result<(), Error> {
        senders.send_to(
            ServiceBus::Ctl,
            self.identity(),
            ServiceId::Farcasterd,
            message,
        )?;
        Ok(())
    }

    fn handle_rpc_msg(
        &mut self,
        senders: &mut Senders,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        match request.clone() {
            Request::Hello => {
                // Ignoring; this is used to set remote identity at ZMQ level
            }
            // 1st protocol message received through peer connection, and last
            // handled by farcasterd, receiving taker commit because we are
            // maker
            Request::Protocol(Msg::TakerCommit(request::TakeCommit {
                commit: remote_commit,
                public_offer_hex,
                swap_id,
            })) => {
                let pub_offer: PublicOffer<BtcXmr> = FromStr::from_str(&public_offer_hex)?;
                trace!(
                    "Offer {} is known, you created it previously, initiating swap with taker",
                    &pub_offer
                );
                let PublicOffer {
                    version,
                    offer,
                    node_id,
                    peer_address,
                } = pub_offer.clone();

                let daemon_service = internet2::RemoteNodeAddr {
                    node_id,
                    remote_addr: internet2::RemoteSocketAddr::Ftcp(peer_address),
                };
                let peer = daemon_service
                    .to_node_addr(internet2::LIGHTNING_P2P_DEFAULT_PORT)
                    .ok_or_else(|| internet2::presentation::Error::InvalidEndpoint)?;
                let external_address = address();
                match offer.maker_role {
                    SwapRole::Bob => {
                        let bob = Bob::<BtcXmr>::new(external_address.into(), FeePriority::Low);
                        let wallet_index = self.wallet_counter.increment();
                        let mut key_manager =
                            KeyManager::new(self.node_secrets.wallet_seed, wallet_index)?;
                        let (local_params, local_proof) =
                            bob.generate_parameters(&mut key_manager, &pub_offer)?;
                        if self.wallets.get(&swap_id).is_none() {
                            let funding = create_funding(&mut key_manager)?;
                            let funding_addr = funding.get_address()?;
                            info!(
                                "Bob, please send Btc to address: {}",
                                &funding_addr.bright_yellow_bold()
                            );
                            info!("Creating {}", "Wallet::Bob".bright_yellow());
                            if let request::Commit::AliceParameters(remote_commit) =
                                remote_commit.clone()
                            {
                                let bob_wallet = BobState::new(
                                    wallet_index,
                                    bob,
                                    local_params.clone(),
                                    local_proof.clone(),
                                    key_manager,
                                    pub_offer.clone(),
                                    Some(funding),
                                    Some(remote_commit),
                                );
                                self.wallets.insert(swap_id, Wallet::Bob(bob_wallet));
                            } else {
                                error!("Not Commit::Alice");
                                return Ok(());
                            }
                            let launch_swap = LaunchSwap {
                                peer: peer.into(),
                                local_trade_role: TradeRole::Maker,
                                public_offer: pub_offer,
                                local_params: Params::Bob(local_params.clone()),
                                // local_proof,
                                swap_id,
                                remote_commit: Some(remote_commit),
                                funding_address: Some(funding_addr),
                            };
                            self.swaps.insert(swap_id, None);
                            self.send_ctl(
                                senders,
                                ServiceId::Farcasterd,
                                Request::LaunchSwap(launch_swap),
                            )?;
                        } else {
                            error!("Wallet already existed");
                        }
                    }
                    SwapRole::Alice => {
                        let alice: Alice<BtcXmr> =
                            Alice::new(external_address.into(), FeePriority::Low);
                        let wallet_seed = self.node_secrets.wallet_seed;
                        let wallet_index = self.wallet_counter.increment();
                        let mut key_manager = KeyManager::new(wallet_seed, wallet_index)?;
                        let (params, proof) =
                            alice.generate_parameters(&mut key_manager, &pub_offer)?;
                        if self.wallets.get(&swap_id).is_none() {
                            info!("Creating {}", "Wallet::Alice".bright_yellow());
                            if let request::Commit::BobParameters(bob_commit) =
                                remote_commit.clone()
                            {
                                self.wallets.insert(
                                    swap_id,
                                    Wallet::Alice(
                                        alice,
                                        params.clone(),
                                        proof.clone(),
                                        key_manager,
                                        pub_offer.clone(),
                                        Some(bob_commit),
                                        None,
                                        None,
                                        None,
                                        None,
                                    ),
                                );

                                let launch_swap = LaunchSwap {
                                    peer: peer.into(),
                                    local_trade_role: TradeRole::Maker,
                                    public_offer: pub_offer,
                                    local_params: Params::Alice(params),
                                    // local_proof: proof,
                                    swap_id,
                                    remote_commit: Some(remote_commit),
                                    funding_address: None,
                                };
                                self.send_ctl(
                                    senders,
                                    ServiceId::Farcasterd,
                                    Request::LaunchSwap(launch_swap),
                                )?;
                                self.send_ctl(
                                    senders,
                                    ServiceId::Farcasterd,
                                    Request::Protocol(Msg::Reveal((swap_id, proof).into())),
                                )?;
                            } else {
                                error!("Not Commit::Bob");
                            }
                        } else {
                            error!("Wallet already existed");
                        }
                    }
                }
            }
            Request::Protocol(Msg::MakerCommit(commit)) => {
                if get_swap_id(source)? != Msg::MakerCommit(commit.clone()).swap_id() {
                    error!("wrong swapid");
                    return Ok(());
                }

                match commit {
                    Commit::BobParameters(CommitBobParameters { swap_id, .. }) => {
                        if let Some(Wallet::Alice(
                            _alice,
                            _alice_params,
                            _alice_proof,
                            _key_manager,
                            _public_offer,
                            bob_commit_params, // None
                            _bob_params,       // None
                            _bob_proof,
                            _core_arb_txs,
                            alice_cancel_sig, // None
                            _,
                        )) = self.wallets.get_mut(&swap_id)
                        {
                            if let Some(_) = bob_commit_params {
                                error!("Bob commit (remote) already set");
                            } else if let Commit::BobParameters(commit) = commit {
                                trace!("Setting bob commit");
                                *bob_commit_params = Some(commit);
                            }
                        } else {
                            error!("Wallet not found or not on correct state");
                            return Ok(());
                        }
                    }
                    Commit::AliceParameters(CommitAliceParameters { swap_id, .. }) => {
                        if let Some(Wallet::Bob(BobState {
                            remote_commit_params, // None
                            ..
                        })) = self.wallets.get_mut(&swap_id)
                        {
                            if let Some(_) = remote_commit_params {
                                error!("Alice commit (remote) already set");
                            } else if let Commit::AliceParameters(commit) = commit {
                                trace!("Setting alice commit");
                                *remote_commit_params = Some(commit);
                            }
                        } else {
                            error!("Wallet not found or not on correct state");
                            return Ok(());
                        }
                    }
                }
            }
            Request::Protocol(Msg::Reveal(reveal)) => {
                let swap_id = get_swap_id(source.clone())?;
                match reveal {
                    // receiving from counterparty Bob, thus I'm Alice (Maker or Taker)
                    Reveal::BobParameters(reveal) => {
                        if let Some(Wallet::Alice(
                            _alice,
                            _alice_params,
                            _alice_proof,
                            key_manager,
                            _public_offer,
                            Some(bob_commit),
                            bob_params,      // None
                            Some(bob_proof), // Should be Some() at this stage
                            _core_arb_txs,
                            alice_cancel_sig,
                            _,
                        )) = self.wallets.get_mut(&swap_id)
                        {
                            if let Some(remote_params) = bob_params {
                                error!("bob_params were previously set to: {}", remote_params);
                                return Ok(());
                            } else {
                                trace!("Setting bob params: {}", reveal);
                                bob_commit.verify_with_reveal(&CommitmentEngine, reveal.clone())?;
                                let remote_params_candidate: BobParameters<BtcXmr> = reveal.into();
                                let proof_verification = key_manager.verify_proof(
                                    &remote_params_candidate.spend,
                                    &remote_params_candidate.adaptor,
                                    bob_proof.proof.clone(), /* remote_params_candidate.proof.
                                                              * clone(), */
                                );

                                if !proof_verification.is_ok() {
                                    error!("DLEQ proof invalid");
                                    return Ok(());
                                }
                                *bob_params = Some(remote_params_candidate);
                                // nothing to do yet, waiting for Msg
                                // CoreArbitratingSetup to proceed
                                return Ok(());
                            }
                        } else {
                            error!("only Some(Wallet::Alice)");
                            return Ok(());
                        }
                    }
                    // getting parameters from counterparty alice routed through
                    // swapd, thus I'm Bob on this swap: Bob can proceed
                    Reveal::AliceParameters(reveal) => {
                        if let Some(Wallet::Bob(BobState {
                            bob,
                            local_params,
                            key_manager,
                            pub_offer,
                            funding_tx: Some(funding_tx),
                            remote_params,                    // None
                            remote_proof: Some(remote_proof), // Some
                            core_arb_setup,                   // None
                            ..
                        })) = self.wallets.get_mut(&swap_id)
                        {
                            // set wallet params
                            if remote_params.is_some() {
                                error!("Alice params already set");
                                return Ok(());
                            }

                            trace!("Setting remote params: {}", reveal);
                            let remote_params_candidate: AliceParameters<BtcXmr> = reveal.into();
                            let proof_verification = key_manager.verify_proof(
                                &remote_params_candidate.spend,
                                &remote_params_candidate.adaptor,
                                remote_proof.proof.clone(),
                            );

                            if !proof_verification.is_ok() {
                                error!("DLEQ proof invalid");
                                return Ok(());
                            }
                            *remote_params = Some(remote_params_candidate);

                            // set wallet core_arb_txs
                            if core_arb_setup.is_some() {
                                error!("Core Arb Txs already set");
                                return Ok(());
                            }
                            if !funding_tx.was_seen() {
                                error!("Funding not yet seen");
                                return Ok(());
                            }
                            // FIXME should be set before
                            let core_arbitrating_txs = bob.core_arbitrating_transactions(
                                &remote_params.clone().expect("alice_params set above"),
                                local_params,
                                funding_tx.clone(),
                                pub_offer,
                            )?;
                            let cosign_arbitrating_cancel =
                                bob.cosign_arbitrating_cancel(key_manager, &core_arbitrating_txs)?;
                            *core_arb_setup = Some(CoreArbitratingSetup::<BtcXmr>::from((
                                swap_id,
                                core_arbitrating_txs,
                                cosign_arbitrating_cancel,
                            )));
                            let core_arb_setup_msg =
                                Msg::CoreArbitratingSetup(core_arb_setup.clone().unwrap());
                            self.send_ctl(senders, source, Request::Protocol(core_arb_setup_msg))?;
                        } else {
                            error!("only Some(Wallet::Bob)");
                        }
                    }
                    Reveal::Proof(reveal) => {
                        match self.wallets.get_mut(&swap_id) {
                            Some(Wallet::Alice(
                                ..,
                                bob_proof, // Should be Some() at this stage
                                _,
                                _,
                            )) => {
                                *bob_proof = Some(Proof {
                                    proof: reveal.proof,
                                });
                                todo!()
                            }
                            Some(Wallet::Bob(BobState {
                                wallet_ix: _,
                                bob: _,
                                local_params: _,
                                local_proof: _,
                                key_manager,
                                pub_offer: _,
                                funding_tx: _,
                                remote_commit_params: _,
                                remote_params,
                                remote_proof,
                                core_arb_setup: _,
                                adaptor_buy: _,
                            })) => {
                                match (remote_params, remote_proof.clone()) {
                                    (None, None) => {
                                        *remote_proof = Some(Proof {
                                            proof: reveal.proof,
                                        });
                                    }
                                    (Some(params), None) => {
                                        let verification_result = key_manager.verify_proof(
                                            &params.spend,
                                            &params.adaptor,
                                            reveal.proof.clone(),
                                        );
                                        if verification_result.is_ok() {
                                            *remote_proof = Some(Proof {
                                                proof: reveal.proof,
                                            });
                                        } else {
                                            error!("DLEQ proof invalid")
                                        }
                                    }
                                    (None, Some(_)) => {
                                        error!("already set DLEQ proof")
                                    }
                                    (Some(_), Some(_)) => {
                                        error!("already set DLEQ proof and parameters")
                                    }
                                };
                            }
                            None => {
                                error!("only Some(Wallet::_)");
                            }
                        }
                    }
                }
            }
            Request::Protocol(Msg::RefundProcedureSignatures(RefundProcedureSignatures {
                swap_id,
                cancel_sig: alice_cancel_sig,
                refund_adaptor_sig,
            })) => {
                let swap_id = get_swap_id(source.clone())?;
                let my_id = self.identity();
                if let Some(Wallet::Bob(BobState {
                    bob,
                    local_params,
                    key_manager,
                    pub_offer,
                    remote_params: Some(remote_params),
                    core_arb_setup: Some(core_arb_setup),
                    adaptor_buy, // None
                    ..
                })) = self.wallets.get_mut(&swap_id)
                {
                    error!("validate_adaptor_refund missing");
                    let core_arb_txs = &(core_arb_setup.clone()).into();
                    let signed_adaptor_refund = &SignedAdaptorRefund { refund_adaptor_sig };

                    bob.validate_adaptor_refund(
                        key_manager,
                        remote_params,
                        local_params,
                        core_arb_txs,
                        signed_adaptor_refund,
                    )?;

                    // *refund_sigs = Some(refund_proc_sigs);
                    {
                        let signed_arb_lock =
                            bob.sign_arbitrating_lock(key_manager, core_arb_txs)?;
                        let sig = signed_arb_lock.lock_sig;
                        let tx = core_arb_setup.lock.clone();
                        let mut lock_tx = LockTx::from_partial(tx);
                        let lock_pubkey = key_manager.get_pubkey(ArbitratingKeyId::Lock)?;
                        lock_tx.add_witness(lock_pubkey, sig)?;
                        let finalized_lock_tx =
                            Broadcastable::<BitcoinSegwitV0>::finalize_and_extract(&mut lock_tx)?;

                        senders.send_to(
                            ServiceBus::Ctl,
                            my_id.clone(),
                            source.clone(), // destination swapd
                            Request::Tx(request::Tx::Lock(finalized_lock_tx)),
                        )?;
                    }

                    {
                        if adaptor_buy.is_some() {
                            error!("adaptor_buy already set");
                            return Ok(());
                        }
                        *adaptor_buy = Some(bob.sign_adaptor_buy(
                            key_manager,
                            remote_params,
                            local_params,
                            core_arb_txs,
                            pub_offer,
                        )?);
                        let buy_proc_sig = BuyProcedureSignature::<BtcXmr>::from((
                            swap_id,
                            adaptor_buy.clone().unwrap(),
                        ));
                        let buy_proc_sig = Msg::BuyProcedureSignature(buy_proc_sig);
                        senders.send_to(
                            ServiceBus::Ctl,
                            my_id.clone(),
                            source.clone(), // destination swapd
                            Request::Protocol(buy_proc_sig),
                        )?;
                    }

                    {
                        // cancel
                        let tx = core_arb_setup.cancel.clone();
                        let mut cancel_tx = CancelTx::from_partial(tx);
                        cancel_tx.add_witness(remote_params.cancel, alice_cancel_sig)?;
                        cancel_tx.add_witness(local_params.cancel, core_arb_setup.cancel_sig)?;
                        let finalized_cancel_tx =
                            Broadcastable::<BitcoinSegwitV0>::finalize_and_extract(&mut cancel_tx)?;
                        senders.send_to(
                            ServiceBus::Ctl,
                            my_id.clone(),
                            source.clone(), // destination swapd
                            Request::Tx(Tx::Cancel(finalized_cancel_tx)),
                        )?;
                    }
                    {
                        // refund
                        let FullySignedRefund {
                            refund_sig,
                            refund_adapted_sig,
                        } = bob.fully_sign_refund(
                            key_manager,
                            core_arb_txs.clone(),
                            signed_adaptor_refund,
                        )?;
                        let tx = core_arb_setup.refund.clone();
                        let mut refund_tx = RefundTx::from_partial(tx);

                        refund_tx.add_witness(local_params.refund, refund_sig)?;
                        refund_tx.add_witness(remote_params.refund, refund_adapted_sig)?;
                        let final_refund_tx =
                            Broadcastable::<BitcoinSegwitV0>::finalize_and_extract(&mut refund_tx)?;
                        senders.send_to(
                            ServiceBus::Ctl,
                            my_id,
                            source, // destination swapd
                            Request::Tx(Tx::Refund(final_refund_tx)),
                        )?;
                    }
                } else {
                    error!("Unknown wallet and swap_id");
                }
            }
            Request::Protocol(Msg::CoreArbitratingSetup(core_arbitrating_setup)) => {
                let swap_id = get_swap_id(source.clone())?;
                if let Some(Wallet::Alice(
                    alice,
                    alice_params,
                    alice_proof,
                    key_manager,
                    public_offer,
                    _bob_commit_params,
                    Some(bob_parameters),
                    Some(bob_proof),
                    core_arb_setup,   // None
                    alice_cancel_sig, // None
                    adaptor_refund,   // None
                )) = self.wallets.get_mut(&swap_id)
                {
                    if core_arb_setup.is_some() {
                        error!("core_arb_txs already set for alice");
                        return Ok(());
                    }
                    if alice_cancel_sig.is_some() {
                        error!("alice_cancel_sig already set for alice");
                        return Ok(());
                    }
                    *core_arb_setup = Some(core_arbitrating_setup.clone());
                    let core_arb_txs: CoreArbitratingTransactions<Bitcoin<SegwitV0>> =
                        core_arbitrating_setup.into();
                    let signed_adaptor_refund = alice.sign_adaptor_refund(
                        key_manager,
                        alice_params,
                        bob_parameters,
                        &core_arb_txs,
                        public_offer,
                    )?;
                    *adaptor_refund = Some(signed_adaptor_refund.clone());
                    let cosigned_arb_cancel = alice.cosign_arbitrating_cancel(
                        key_manager,
                        alice_params,
                        bob_parameters,
                        &core_arb_txs,
                        public_offer,
                    )?;
                    let refund_proc_signatures = RefundProcedureSignatures::from((
                        swap_id,
                        cosigned_arb_cancel,
                        signed_adaptor_refund,
                    ));
                    *alice_cancel_sig = Some(refund_proc_signatures.cancel_sig);

                    {
                        let FullySignedPunish { punish, punish_sig } = alice.fully_sign_punish(
                            key_manager,
                            alice_params,
                            bob_parameters,
                            &core_arb_txs,
                            public_offer,
                        )?;

                        let mut punish_tx = PunishTx::from_partial(punish);
                        punish_tx.add_witness(alice_params.punish, punish_sig)?;
                        let tx =
                            Broadcastable::<BitcoinSegwitV0>::finalize_and_extract(&mut punish_tx)?;
                        senders.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            source.clone(),
                            Request::Tx(Tx::Punish(tx)),
                        )?;
                    }

                    let refund_proc_signatures =
                        Msg::RefundProcedureSignatures(refund_proc_signatures);

                    senders.send_to(
                        ServiceBus::Ctl,
                        self.identity(),
                        source,
                        Request::Protocol(refund_proc_signatures),
                    )?
                } else {
                    error!("only Some(Wallet::Alice)");
                }
            }
            Request::Protocol(Msg::BuyProcedureSignature(BuyProcedureSignature {
                swap_id,
                buy,
                buy_adaptor_sig: buy_encrypted_sig,
            })) => {
                info!("wallet received buyproceduresignature");
                let signed_adaptor_buy = SignedAdaptorBuy {
                    buy: buy.clone(),
                    buy_adaptor_sig: buy_encrypted_sig,
                };
                if get_swap_id(source.clone())? != swap_id {
                    error!("wrong swapid");
                    return Ok(());
                };
                let id = self.identity();
                if let Some(Wallet::Alice(
                    alice,
                    alice_params,
                    alice_proof,
                    key_manager,
                    public_offer,
                    _bob_commit_params,
                    Some(bob_parameters),
                    Some(bob_proof),
                    Some(core_arb_setup),
                    Some(alice_cancel_sig),
                    _,
                )) = self.wallets.get_mut(&swap_id)
                {
                    let core_arb_txs = &(core_arb_setup.clone()).into();

                    // cancel
                    let tx = core_arb_setup.cancel.clone();
                    let mut cancel_tx = CancelTx::from_partial(tx);
                    cancel_tx.add_witness(alice_params.cancel, alice_cancel_sig.clone())?;
                    cancel_tx.add_witness(bob_parameters.cancel, core_arb_setup.cancel_sig)?;
                    let finalized_cancel_tx =
                        Broadcastable::<BitcoinSegwitV0>::finalize_and_extract(&mut cancel_tx)?;
                    senders.send_to(
                        ServiceBus::Ctl,
                        id.clone(),
                        source.clone(), // destination swapd
                        Request::Tx(Tx::Cancel(finalized_cancel_tx)),
                    )?;

                    // buy
                    let mut buy_tx = BuyTx::from_partial(buy);
                    alice.validate_adaptor_buy(
                        key_manager,
                        alice_params,
                        bob_parameters,
                        core_arb_txs,
                        public_offer,
                        &signed_adaptor_buy,
                    )?;
                    let FullySignedBuy {
                        buy_sig,
                        buy_adapted_sig: buy_decrypted_sig,
                    } = alice.fully_sign_buy(
                        key_manager,
                        alice_params,
                        bob_parameters,
                        core_arb_txs,
                        public_offer,
                        &signed_adaptor_buy,
                    )?;
                    buy_tx.add_witness(key_manager.get_pubkey(ArbitratingKeyId::Buy)?, buy_sig)?;
                    buy_tx.add_witness(bob_parameters.buy, buy_decrypted_sig)?;
                    let tx = Broadcastable::<BitcoinSegwitV0>::finalize_and_extract(&mut buy_tx)?;
                    trace!("wallet sends fullysignedbuy");
                    senders.send_to(ServiceBus::Ctl, id, source, Request::Tx(Tx::Buy(tx)))?;

                    // buy_adaptor_sig
                } else {
                    error!("could not get alice's wallet")
                }
            }
            _ => {
                error!("MSG RPC can only be used for forwarding LNPBP messages")
            }
        }
        Ok(())
    }

    fn handle_rpc_ctl(
        &mut self,
        senders: &mut Senders,
        source: ServiceId,
        request: Request,
    ) -> Result<(), Error> {
        match request {
            Request::Hello => match &source {
                ServiceId::Swap(swap_id) => {
                    if let Some(option_req) = self.swaps.get_mut(&swap_id) {
                        trace!("Known swapd, you launched it");
                        if let Some(req) = option_req {
                            let request = req.clone();
                            *option_req = None;
                            self.send_ctl(senders, source, request)?
                        }
                    }
                }
                source => {
                    debug!("Received Hello from {}", source);
                }
            },
            Request::Progress(progress) => {
                // TODO update wallet state?
                info!("{}", progress);
            }

            Request::TakeOffer(request::PubOffer {
                public_offer,
                peer_secret_key: None,
            }) => {
                let PublicOffer {
                    version,
                    offer,
                    node_id,
                    peer_address,
                } = public_offer.clone();
                let daemon_service = internet2::RemoteNodeAddr {
                    node_id,                                                      // checked above
                    remote_addr: internet2::RemoteSocketAddr::Ftcp(peer_address), /* expected RemoteSocketAddr */
                };
                let peer = daemon_service
                    .to_node_addr(LIGHTNING_P2P_DEFAULT_PORT)
                    .ok_or_else(|| internet2::presentation::Error::InvalidEndpoint)?;
                let peer = daemon_service
                    .to_node_addr(LIGHTNING_P2P_DEFAULT_PORT)
                    .ok_or_else(|| internet2::presentation::Error::InvalidEndpoint)?;

                let swap_id: SwapId = SwapId::random().into();
                self.swaps.insert(swap_id, None);
                // since we're takers, we are on the other side of the trade
                let taker_role = offer.maker_role.other();
                let wallet_index = self.wallet_counter.increment();
                let mut key_manager = KeyManager::new(self.node_secrets.wallet_seed, wallet_index)?;
                let external_address = address();
                match taker_role {
                    SwapRole::Bob => {
                        let bob: Bob<BtcXmr> = Bob::new(external_address.into(), FeePriority::Low);
                        let (local_params, local_proof) =
                            bob.generate_parameters(&mut key_manager, &public_offer)?;
                        let funding = create_funding(&mut key_manager)?;
                        let funding_addr = funding.get_address()?;
                        let funding_fee = 150;
                        let funding_amount = offer.arbitrating_amount.as_sat() + funding_fee;
                        info!(
                            "Send {} sats to address: {}",
                            funding_amount.bright_green_bold(),
                            funding_addr.addr(),
                        );
                        info!("Creating {}", "Wallet::Bob".bright_yellow());
                        if self.wallets.get(&swap_id).is_none() {
                            let local_wallet = BobState::new(
                                self.wallet_counter.increment(),
                                bob,
                                local_params.clone(),
                                local_proof.clone(),
                                key_manager,
                                public_offer.clone(),
                                Some(funding),
                                None,
                            );
                            self.wallets.insert(swap_id, Wallet::Bob(local_wallet));
                        } else {
                            error!("Wallet already exists");
                            return Ok(());
                        }
                        let launch_swap = LaunchSwap {
                            peer: peer.into(),
                            local_trade_role: TradeRole::Taker,
                            public_offer,
                            local_params: Params::Bob(local_params),
                            // local_proof,
                            swap_id,
                            remote_commit: None,
                            funding_address: Some(funding_addr),
                        };
                        senders.send_to(
                            ServiceBus::Ctl,
                            source,
                            ServiceId::Farcasterd,
                            Request::LaunchSwap(launch_swap),
                        )?;
                        self.send_ctl(
                            senders,
                            ServiceId::Farcasterd,
                            Request::Protocol(Msg::Reveal((swap_id, local_proof).into())),
                        )?;
                    }
                    SwapRole::Alice => {
                        let alice: Alice<BtcXmr> =
                            Alice::new(external_address.into(), FeePriority::Low);
                        let (local_params, local_proof) =
                            alice.generate_parameters(&mut key_manager, &public_offer)?;
                        let wallet_seed = self.node_secrets.wallet_seed;
                        let wallet_index = self.wallet_counter.increment();
                        let key_manager = KeyManager::new(wallet_seed, wallet_index)?;

                        if self.wallets.get(&swap_id).is_none() {
                            // TODO instead of storing in state, start building
                            // requests and store the state in there directly
                            info!("Creating Alice Taker's Wallet");
                            self.wallets.insert(
                                swap_id,
                                Wallet::Alice(
                                    alice,
                                    local_params.clone(),
                                    local_proof.clone(),
                                    key_manager,
                                    public_offer.clone(),
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                ),
                            );
                        } else {
                            error!("Wallet already exists");
                        }
                        let launch_swap = LaunchSwap {
                            peer: peer.into(),
                            local_trade_role: TradeRole::Taker,
                            public_offer,
                            local_params: Params::Alice(local_params),
                            // local_proof,
                            swap_id,
                            remote_commit: None,
                            funding_address: None,
                        };
                        senders.send_to(
                            ServiceBus::Ctl,
                            source,
                            ServiceId::Farcasterd,
                            Request::LaunchSwap(launch_swap),
                        )?;
                    }
                };
            }
            Request::Tx(Tx::Funding(tx)) => {
                if let Some(Wallet::Bob(BobState {
                    funding_tx: Some(funding),
                    ..
                })) = self.wallets.get_mut(&get_swap_id(source.clone())?)
                {
                    if funding.was_seen() {
                        warn!("funding was previously updated, ignoring");
                        return Ok(());
                    }
                    funding_update(funding, tx)?;
                    info!("bob's wallet informs swapd that funding was successfully updated");
                    senders.send_to(
                        ServiceBus::Ctl,
                        ServiceId::Wallet,
                        source,
                        Request::FundingUpdated,
                    )?;
                }
            }
            Request::Tx(Tx::Buy(buy_tx)) => {
                if let Some(Wallet::Bob(BobState {
                    bob,
                    key_manager,
                    remote_params: Some(alice_params),
                    core_arb_setup: Some(_),
                    adaptor_buy: Some(adaptor_buy),
                    ..
                })) = self.wallets.get_mut(&get_swap_id(source.clone())?)
                {
                    let sk_a_btc = bob.recover_accordant_assets(
                        key_manager,
                        alice_params,
                        adaptor_buy.clone(),
                        buy_tx,
                    );
                    let mut sk_a_btc_buf: Vec<u8> = (*sk_a_btc.as_ref()).into();
                    sk_a_btc_buf.reverse();
                    let sk_a = monero::PrivateKey::from_slice(sk_a_btc_buf.as_ref())
                        .expect("Valid Monero Private Key");
                    info!("Extracted alice's monero key from Buy tx: {}", sk_a);

                    let sk_b = key_manager.get_or_derive_monero_spend_key()?;
                    info!("Full secret monero spending key: {}", sk_a + sk_b);

                    let view_key = *alice_params
                        .accordant_shared_keys
                        .clone()
                        .into_iter()
                        .find(|vk| vk.tag() == &SharedKeyId::new(SHARED_VIEW_KEY_ID))
                        .unwrap()
                        .elem();
                    info!("Full secret monero view key: {}", view_key);
                }
            }
            Request::Tx(Tx::Refund(refund_tx)) => {
                if let Some(Wallet::Alice(
                    alice,
                    _,
                    key_manager,
                    _,
                    _,
                    Some(bob_params), //remote
                    Some(_),
                    _,
                    Some(adaptor_refund),
                )) = self.wallets.get_mut(&get_swap_id(source.clone())?)
                {
                    let sk_b_btc = alice.recover_accordant_assets(
                        key_manager,
                        bob_params,
                        adaptor_refund.clone(),
                        refund_tx,
                    );
                    let mut sk_b_btc_buf: Vec<u8> = (*sk_b_btc.as_ref()).into();
                    sk_b_btc_buf.reverse();
                    let sk_b = monero::PrivateKey::from_slice(sk_b_btc_buf.as_ref())
                        .expect("Valid Monero Private Key");
                    info!("Extracted alice's monero key from Buy tx: {}", sk_b);

                    let sk_c = key_manager.get_or_derive_monero_spend_key()?;
                    info!("Full secret monero spending key: {}", sk_b + sk_c);

                    let view_key = *bob_params
                        .accordant_shared_keys
                        .clone()
                        .into_iter()
                        .find(|vk| vk.tag() == &SharedKeyId::new(SHARED_VIEW_KEY_ID))
                        .unwrap()
                        .elem();
                    info!("Full secret monero view key: {}", view_key);
                }
            }
            Request::GetKeys(request::GetKeys(wallet_token, request_id)) => {
                if wallet_token != self.wallet_token {
                    Err(Error::InvalidToken)?
                }
                info!("sent Secret request to farcasterd");
                self.send_farcasterd(
                    senders,
                    Request::Keys(Keys(
                        self.node_secrets.peerd_secret_key,
                        self.node_secrets.node_id(),
                        request_id,
                    )),
                )?
            }

            _ => {
                error!(
                    "Request {:?} is not supported by the CTL interface",
                    request
                );
            }
        }
        Ok(())
    }
}

fn address() -> bitcoin::Address {
    let mut address = bitcoin::Address::from_str("tb1qa83aeqmfvn23llr2zc3gfkrwt8xvpv2k2cluzg")
        .expect("Parsable address");
    let path = &"./addresses.bitcoin";
    // use address from first line and drop that line
    match set_addr(&mut address, path) {
        Ok(()) => address,
        Err(e) => {
            error!(
                "make sure {} exists populated with bitcoin addresses you control \
                 such as: \n\ntb1qh0hgmalfuancfe28wnmrp0lctlsdxqf2fcqlsh\n\
                 tb1qgd0m0qwssw7q8t8whh2d0ala0g8xqfq976y9eu\n\n\n\
                 with no extra spaces, now defaulting to hardcoded address {} \
                 {}",
                path, address, e
            );
            address
        }
    }
}

fn set_addr(address: &mut bitcoin::Address, path: &str) -> io::Result<()> {
    let updated_lines = read_lines(path)?
        .enumerate()
        .filter_map(|(ix, line)| {
            let addr = line.ok()?;
            if ix == 0 {
                *address = bitcoin::Address::from_str(&addr).ok()?;
                // consume 1st line
                None
            } else {
                Some(addr)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    std::fs::File::create(path)?.write_all(updated_lines.as_ref())?;
    Ok(())
}

// The output is wrapped in a Result to allow matching on errors
// Returns an Iterator to the Reader of the lines of the file.
fn read_lines<P>(filename: P) -> io::Result<io::Lines<io::BufReader<std::fs::File>>>
where
    P: AsRef<std::path::Path>,
{
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(filename)?;
    Ok(io::BufRead::lines(io::BufReader::new(file)))
}

pub fn create_funding(key_manager: &mut KeyManager) -> Result<FundingTx, Error> {
    let pk = key_manager.get_pubkey(ArbitratingKeyId::Lock)?;
    Ok(FundingTx::initialize(
        pk,
        farcaster_core::blockchain::Network::Testnet,
    )?)
}

pub fn funding_update(funding: &mut FundingTx, tx: bitcoin::Transaction) -> Result<(), Error> {
    let funding_bundle = FundingTransaction::<Bitcoin<SegwitV0>> { funding: tx };
    Ok(funding.update(funding_bundle.funding.clone())?)
}
