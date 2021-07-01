use std::{collections::HashMap, str::FromStr};

use crate::rpc::{Request, ServiceBus, request::{self, Keypair, Msg, Params, RuntimeContext}};
use crate::walletd::NodeSecrets;
use crate::LogStyle;
use crate::Senders;
use crate::{Config, CtlServer, Error, Service, ServiceId};
use bitcoin::secp256k1;
use farcaster_chains::{
    bitcoin::{Bitcoin, Wallet as BTCWallet},
    monero::{Monero, Wallet as XMRWallet},
    pairs::btcxmr::BtcXmr,
};
use farcaster_core::{
    blockchain::FeePolitic,
    bundle::{AliceParameters, BobParameters, CoreArbitratingTransactions, FundingTransaction},
    negotiation::PublicOffer,
    protocol_message::{BuyProcedureSignature, CoreArbitratingSetup, RefundProcedureSignatures},
    role::{Alice, Bob, NegotiationRole, SwapRole},
};
use internet2::{LocalNode, ToNodeAddr, TypedEnum, LIGHTNING_P2P_DEFAULT_PORT};
use lnp::{ChannelId as SwapId, TempChannelId as TempSwapId};
use microservices::esb::{self, Handler};
use request::{LaunchSwap, NodeId, Secret};

pub fn run(
    config: Config,
    walletd_token: String,
    node_secrets: NodeSecrets,
    node_id: bitcoin::secp256k1::PublicKey,
) -> Result<(), Error> {
    let runtime = Runtime {
        identity: ServiceId::Wallet,
        walletd_token,
        node_secrets,
        node_id,
        wallets: none!(),
    };

    Service::run(config, runtime, false)
}

pub struct Runtime {
    identity: ServiceId,
    walletd_token: String,
    node_secrets: NodeSecrets,
    node_id: bitcoin::secp256k1::PublicKey,
    wallets: HashMap<SwapId, Wallet>,
}

pub enum Wallet {
    Alice(
        Alice<BtcXmr>,
        AliceParameters<BtcXmr>,
        BTCWallet,
        XMRWallet,
        PublicOffer<BtcXmr>,
        Option<BobParameters<BtcXmr>>,
    ),
    Bob(
        Bob<BtcXmr>,
        BobParameters<BtcXmr>,
        BTCWallet,
        XMRWallet,
        PublicOffer<BtcXmr>,
        Option<FundingTransaction<Bitcoin>>,
        Option<AliceParameters<BtcXmr>>,
        Option<CoreArbitratingTransactions<Bitcoin>>,
    ),
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

    fn swap_id(&self, source: ServiceId) -> Result<SwapId, Error> {
        if let ServiceId::Swap(swap_id) = source {
            Ok(swap_id)
        } else {
            Err(Error::Farcaster("Not swapd".to_string()))
        }
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
                commit,
                public_offer_hex,
                swap_id,
            })) => {
                let public_offer: PublicOffer<BtcXmr> = FromStr::from_str(&public_offer_hex)
                    .map_err(|_| {
                        Error::Farcaster(
                            "The offer received on peer conection is not parsable".to_string(),
                        )
                    })?;
                trace!(
                    "Offer {} is known, you created it previously, initiating swap with taker",
                    &public_offer
                );
                let PublicOffer {
                    version,
                    offer,
                    daemon_service,
                } = public_offer.clone();
                let peer = daemon_service
                    .to_node_addr(internet2::LIGHTNING_P2P_DEFAULT_PORT)
                    .ok_or_else(|| internet2::presentation::Error::InvalidEndpoint)?;
                match offer.maker_role {
                    SwapRole::Bob => {
                        let external_address = bitcoin::Address::from_str(
                            "bc1qesgvtyx9y6lax0x34napc2m7t5zdq6s7xxwpvk",
                        )
                        .expect("Parsable address");
                        let bob =
                            Bob::<BtcXmr>::new(external_address.into(), FeePolitic::Aggressive);
                        let wallet_seed = self.node_secrets.wallet_seed;
                        let btc_wallet = BTCWallet::new(wallet_seed);
                        let xmr_wallet = XMRWallet::new(wallet_seed);
                        let params =
                            bob.generate_parameters(&btc_wallet, &xmr_wallet, &public_offer)?;
                        if self.wallets.get(&swap_id).is_none() {
                            self.wallets.insert(
                                swap_id,
                                Wallet::Bob(
                                    bob,
                                    params.clone(),
                                    btc_wallet,
                                    xmr_wallet,
                                    public_offer.clone(),
                                    None,
                                    None,
                                    None,
                                ),
                            );
                            let launch_swap = LaunchSwap {
                                peerd: peer.into(),
                                negotiation_role: NegotiationRole::Maker,
                                public_offer,
                                params: Params::Bob(params),
                                swap_id,
                                commit: Some(commit),
                            };
                            senders.send_to(
                                ServiceBus::Ctl,
                                source,
                                ServiceId::Farcasterd,
                                Request::LaunchSwap(launch_swap),
                            )?;
                        } else {
                            Err(Error::Farcaster("Wallet already existed".to_string()))?
                        }
                    }
                    SwapRole::Alice => {
                        let external_address = bitcoin::Address::from_str(
                            "bc1qesgvtyx9y6lax0x34napc2m7t5zdq6s7xxwpvk",
                        )
                        .expect("Parsable address");
                        let alice: Alice<BtcXmr> =
                            Alice::new(external_address.into(), FeePolitic::Aggressive);
                        let wallet_seed = self.node_secrets.wallet_seed;
                        let btc_wallet = BTCWallet::new(wallet_seed);
                        let xmr_wallet = XMRWallet::new(wallet_seed);
                        let params =
                            alice.generate_parameters(&btc_wallet, &xmr_wallet, &public_offer)?;
                        if self.wallets.get(&swap_id).is_none() {
                            self.wallets.insert(
                                swap_id,
                                Wallet::Alice(
                                    alice,
                                    params.clone(),
                                    btc_wallet,
                                    xmr_wallet,
                                    public_offer.clone(),
                                    None,
                                ),
                            );
                            let launch_swap = LaunchSwap {
                                peerd: peer.into(),
                                negotiation_role: NegotiationRole::Maker,
                                public_offer,
                                params: Params::Alice(params),
                                swap_id,
                                commit: Some(commit),
                            };
                            senders.send_to(
                                ServiceBus::Ctl,
                                source,
                                ServiceId::Farcasterd,
                                Request::LaunchSwap(launch_swap),
                            )?;
                        } else {
                            error!("Wallet already existed");
                            Err(Error::Farcaster("Wallet already existed".to_string()))?
                        }
                    }
                };
            }
            _ => {
                error!("MSG RPC can only be used for farwarding LNPBP messages")
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
            Request::Protocol(Msg::RefundProcedureSignatures(refund_proc_sigs)) => {
                let swap_id = self.swap_id(source.clone())?;

                match self.wallets.get_mut(&swap_id) {
                    Some(Wallet::Bob(
                        bob,
                        bob_params,
                        btc_wallet,
                        _xmr_wallet,
                        public_offer,
                        Some(_funding_tx),
                        Some(alice_params),
                        Some(core_arbitrating_txs),
                    )) => {
                        // *refund_sigs = Some(refund_proc_sigs);
                        let signed_adaptor_buy = bob.sign_adaptor_buy(
                            &btc_wallet,
                            alice_params,
                            bob_params,
                            core_arbitrating_txs,
                            public_offer,
                        )?;
                        let signed_arb_lock = bob.sign_arbitrating_lock(
                            &btc_wallet,
                            &btc_wallet,
                            core_arbitrating_txs,
                        )?;

                        // TODO: here subscribe to all transactions with syncerd, and publish lock
                        let buy_proc_sig =
                            BuyProcedureSignature::<BtcXmr>::from_bundle(&signed_adaptor_buy)?;
                        let buy_proc_sig = Msg::BuyProcedureSignature(buy_proc_sig);
                        senders.send_to(
                            ServiceBus::Ctl,
                            ServiceId::Farcasterd,
                            source, // destination swapd
                            Request::Protocol(buy_proc_sig),
                        )?
                    }
                    _ => Err(Error::Farcaster("Unknow wallet and swap_id".to_string()))?,
                }
            }
            Request::Protocol(Msg::CoreArbitratingSetup(core_arb_setup)) => {
                let swap_id = self.swap_id(source.clone())?;
                let core_arb_txs = core_arb_setup.into_core_transactions();
                match self.wallets.get(&swap_id) {
                    Some(Wallet::Alice(
                        alice,
                        alice_params,
                        btc_wallet,
                        xmr_wallet,
                        public_offer,
                        Some(bob_parameters),
                    )) => {
                        let signed_adaptor_refund = alice.sign_adaptor_refund(
                            &btc_wallet,
                            alice_params,
                            bob_parameters,
                            &core_arb_txs,
                            public_offer,
                        )?;
                        let cosigned_arb_cancel = alice.cosign_arbitrating_cancel(
                            &btc_wallet,
                            alice_params,
                            bob_parameters,
                            &core_arb_txs,
                            public_offer,
                        )?;
                        let refund_proc_signatures = RefundProcedureSignatures::from_bundles(
                            &cosigned_arb_cancel,
                            &signed_adaptor_refund,
                        )?;
                        let refund_proc_signatures =
                            Msg::RefundProcedureSignatures(refund_proc_signatures);

                        senders.send_to(
                            ServiceBus::Ctl,
                            ServiceId::Farcasterd,
                            source,
                            Request::Protocol(refund_proc_signatures),
                        )?
                    }
                    _ => Err(Error::Farcaster("only Wallet::Alice".to_string()))?,
                }
            }
            Request::TakeOffer(public_offer) => {
                let PublicOffer {
                    version,
                    offer,
                    daemon_service,
                } = public_offer.clone();
                let peer = daemon_service
                    .to_node_addr(LIGHTNING_P2P_DEFAULT_PORT)
                    .ok_or_else(|| internet2::presentation::Error::InvalidEndpoint)?;

                let swap_id: SwapId = TempSwapId::random().into(); // TODO: replace by public_offer_id
                                                                   // since we're takers, we are on the other side
                let taker_role = offer.maker_role.other();
                let wallet_seed = self.node_secrets.wallet_seed;
                let btc_wallet = BTCWallet::new(wallet_seed);
                let xmr_wallet = XMRWallet::new(wallet_seed);
                match taker_role {
                    SwapRole::Bob => {
                        let address = bitcoin::Address::from_str(
                            "bc1qesgvtyx9y6lax0x34napc2m7t5zdq6s7xxwpvk",
                        )
                        .expect("Parsable address");
                        let bob: Bob<BtcXmr> = Bob::new(address.into(), FeePolitic::Aggressive);
                        let params =
                            bob.generate_parameters(&btc_wallet, &xmr_wallet, &public_offer)?;

                        let launch_swap = LaunchSwap {
                            peerd: peer.into(),
                            negotiation_role: NegotiationRole::Taker,
                            public_offer,
                            params: Params::Bob(params),
                            swap_id,
                            commit: None,
                        };
                        senders.send_to(
                            ServiceBus::Ctl,
                            source,
                            ServiceId::Farcasterd,
                            Request::LaunchSwap(launch_swap),
                        )?;
                    }
                    SwapRole::Alice => {
                        let address = bitcoin::Address::from_str(
                            "bc1qesgvtyx9y6lax0x34napc2m7t5zdq6s7xxwpvk",
                        )
                        .expect("Parsable address");
                        let alice: Alice<BtcXmr> =
                            Alice::new(address.into(), FeePolitic::Aggressive);
                        let params =
                            alice.generate_parameters(&btc_wallet, &xmr_wallet, &public_offer)?;

                        let launch_swap = LaunchSwap {
                            peerd: peer.into(),
                            negotiation_role: NegotiationRole::Taker,
                            public_offer,
                            params: Params::Alice(params),
                            swap_id,
                            commit: None,
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
            Request::Params(params) => {
                let swap_id = if let ServiceId::Swap(swap_id) = source {
                    Ok(swap_id)
                } else {
                    Err(Error::Farcaster("Unknown swap_id".to_string()))
                }?;
                match params {
                    // getting paramaters from counterparty alice routed through
                    // swapd, thus im bob on this swap
                    Params::Alice(params) => {
                        match self.wallets.get_mut(&swap_id) {
                            Some(Wallet::Bob(
                                bob,
                                bob_params,
                                btc_wallet,
                                xmr_wallet,
                                public_offer,
                                // TODO: set funding_bundle somewhere, its now
                                // actually None, so will never hit this.
                                Some(funding_bundle),
                                alice_params, // None
                                core_arb_txs, // None
                            )) => {
                                // set wallet params
                                if alice_params.is_some() {
                                    Err(Error::Farcaster("Alice params already set".to_string()))?
                                }
                                *alice_params = Some(params.clone());

                                // set wallet core_arb_txs
                                if core_arb_txs.is_some() {
                                    Err(Error::Farcaster("Core Arb Txs already set".to_string()))?
                                }
                                let core_arbitrating_txs = bob.core_arbitrating_transactions(
                                    &params,
                                    bob_params,
                                    &funding_bundle,
                                    public_offer,
                                )?;
                                *core_arb_txs = Some(core_arbitrating_txs.clone());
                                let cosign_arbitrating_cancel = bob.cosign_arbitrating_cancel(
                                    &btc_wallet,
                                    bob_params,
                                    &core_arbitrating_txs,
                                )?;
                                let core_arb_setup = CoreArbitratingSetup::<BtcXmr>::from_bundles(
                                    &core_arbitrating_txs,
                                    &cosign_arbitrating_cancel,
                                )?;
                                let core_arb_setup = Msg::CoreArbitratingSetup(core_arb_setup);
                                senders.send_to(
                                    ServiceBus::Ctl,
                                    ServiceId::Farcasterd, // source
                                    source,                // destination swapd
                                    Request::Protocol(core_arb_setup),
                                )?
                            }
                            _ => Err(Error::Farcaster("only Some(Wallet::Bob)".to_string()))?,
                        }
                    }

                    // getting paramaters from counterparty bob, thus im alice
                    // on this swap
                    Params::Bob(params) => match self.wallets.get_mut(&swap_id) {
                        Some(Wallet::Alice(
                            _alice,
                            _alice_params,
                            _,
                            _,
                            _public_offer,
                            bob_params,
                        )) => {
                            *bob_params = Some(params);
                        }
                        _ => Err(Error::Farcaster("only Some(Wallet::Alice)".to_string()))?,
                    },
                }
            }
            Request::GetPeerSecret(request::GetPeerSecret(walletd_token, context)) => {
                if walletd_token != self.walletd_token {
                    Err(Error::InvalidToken)?
                }
                info!("sent Secret request to farcasterd");
                self.send_farcasterd(
                    senders,
                    Request::Keypair(Keypair(
                        self.node_secrets.peerd_secret_key,
                        self.node_secrets.node_id(),
                    )),
                )?
            }
            Request::GetNodeId => {
                let node_id = NodeId(self.node_id.clone());
                self.send_farcasterd(senders, Request::NodeId(node_id))?
            }

            Request::Loopback(request) => match request {
                RuntimeContext::GetInfo => self.send_farcasterd(senders, Request::GetInfo)?,
                RuntimeContext::MakeOffer(offer) => {
                    self.send_farcasterd(senders, Request::MakeOffer(offer))?
                }
                RuntimeContext::TakeOffer(offer) => {
                    self.send_farcasterd(senders, Request::TakeOffer(offer))?
                }
                RuntimeContext::Listen(addr) => {
                    self.send_farcasterd(senders, Request::Listen(addr))?
                }
                RuntimeContext::ConnectPeer(addr) => {
                    self.send_farcasterd(senders, Request::ConnectPeer(addr))?
                }
            },

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
