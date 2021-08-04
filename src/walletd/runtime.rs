use std::{
    collections::{HashMap, HashSet},
    convert::{TryFrom, TryInto},
    str::FromStr,
};

use crate::rpc::{
    request::{self, Keys, Msg, Params, Reveal, Token},
    Request, ServiceBus,
};
use crate::swapd::swap_id;
use crate::walletd::NodeSecrets;
use crate::LogStyle;
use crate::Senders;
use crate::{Config, CtlServer, Error, Service, ServiceId};
use bitcoin::{hashes::hex::FromHex, secp256k1, util::psbt::serialize::Deserialize, Transaction};
use colored::Colorize;
use farcaster_core::{
    blockchain::FeePolitic,
    bundle::{AliceParameters, BobParameters, CoreArbitratingTransactions, FundingTransaction},
    chain::bitcoin::{transaction::Funding, Bitcoin},
    chain::monero::Monero,
    chain::pairs::btcxmr::{BtcXmr, Wallet as CoreWallet},
    negotiation::PublicOffer,
    protocol_message::{
        BuyProcedureSignature, CommitAliceParameters, CoreArbitratingSetup,
        RefundProcedureSignatures,
    },
    role::{Alice, Bob, SwapRole, TradeRole},
    swap::SwapId,
    transaction::Fundable,
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
}

pub enum Wallet {
    Alice(
        Alice<BtcXmr>,
        AliceParameters<BtcXmr>,
        CoreWallet,
        PublicOffer<BtcXmr>,
        Option<BobParameters<BtcXmr>>,
    ),
    Bob(
        Bob<BtcXmr>,
        BobParameters<BtcXmr>,
        CoreWallet,
        PublicOffer<BtcXmr>,
        Option<Funding>,
        Option<CommitAliceParameters<BtcXmr>>,
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
                        let external_address = address();
                        let bob =
                            Bob::<BtcXmr>::new(external_address.into(), FeePolitic::Aggressive);
                        let core_wallet = CoreWallet::new(self.node_secrets.wallet_seed);
                        let local_params = bob.generate_parameters(&core_wallet, &public_offer)?;
                        if self.wallets.get(&swap_id).is_none() {
                            let funding = create_funding()?;
                            info!("Creating {}", "Wallet::Bob".bright_yellow());
                            if let request::Commit::Alice(remote_commit) = remote_commit.clone() {
                                if self.wallets.get(&swap_id).is_none() {
                                    self.wallets.insert(
                                        swap_id,
                                        Wallet::Bob(
                                            bob,
                                            local_params.clone(),
                                            core_wallet,
                                            public_offer.clone(),
                                            Some(funding),
                                            Some(remote_commit),
                                            None,
                                            None,
                                        ),
                                    );
                                } else {
                                    error!("Wallet already exists");
                                    return Ok(());
                                }
                            } else {
                                error!("Not Commit::Alice");
                                return Ok(());
                            }
                            let launch_swap = LaunchSwap {
                                peer: peer.into(),
                                local_trade_role: TradeRole::Maker,
                                public_offer,
                                local_params: Params::Bob(local_params.clone()),
                                swap_id,
                                remote_commit: Some(remote_commit),
                            };
                            self.swaps.insert(swap_id, None);
                            self.send_ctl(
                                senders,
                                ServiceId::Farcasterd,
                                Request::LaunchSwap(launch_swap),
                            )
                        } else {
                            Err(Error::Farcaster("Wallet already existed".to_string()))
                        }
                    }
                    SwapRole::Alice => {
                        let external_address = address();
                        let alice: Alice<BtcXmr> =
                            Alice::new(external_address.into(), FeePolitic::Aggressive);
                        let wallet_seed = self.node_secrets.wallet_seed;
                        let core_wallet = CoreWallet::new(wallet_seed);
                        let params = alice.generate_parameters(&core_wallet, &public_offer)?;
                        if self.wallets.get(&swap_id).is_none() {
                            info!("Creating {}", "Wallet::Alice".bright_yellow());
                            if self.wallets.get(&swap_id).is_none() {
                                self.wallets.insert(
                                    swap_id,
                                    Wallet::Alice(
                                        alice,
                                        params.clone(),
                                        core_wallet,
                                        public_offer.clone(),
                                        None,
                                    ),
                                )
                            } else {
                                error!("Wallet already exists");
                                return Ok(());
                            };
                            let launch_swap = LaunchSwap {
                                peer: peer.into(),
                                local_trade_role: TradeRole::Maker,
                                public_offer,
                                local_params: Params::Alice(params),
                                swap_id,
                                remote_commit: Some(remote_commit),
                            };
                            self.send_ctl(
                                senders,
                                ServiceId::Farcasterd,
                                Request::LaunchSwap(launch_swap),
                            )
                        } else {
                            error!("Wallet already existed");
                            return Ok(());
                        }
                    }
                }?
            }
            Request::Params(role) => {
                let swap_id = swap_id(source.clone())?;
                match role {
                    // receiving from counterparty Bob, thus im Alice Maker or Taker
                    Params::Bob(params) => match self.wallets.get_mut(&swap_id) {
                        Some(Wallet::Alice(
                            _alice,
                            _alice_params,
                            _,
                            _public_offer,
                            bob_params,
                        )) => {
                            *bob_params = Some(params);
                            // nothing to do yet, waiting for Msg
                            // CoreArbitratingSetup to proceed
                        }
                        _ => Err(Error::Farcaster("only Some(Wallet::Alice)".to_string()))?,
                    },
                    // getting paramaters from counterparty alice routed through
                    // swapd, thus im bob on this swap, Bob can proceed
                    Params::Alice(params) => {
                        match self.wallets.get_mut(&swap_id) {
                            Some(Wallet::Bob(
                                bob,
                                bob_params,
                                core_wallet,
                                public_offer,
                                // TODO: set funding_bundle somewhere, its now
                                Some(funding),
                                Some(commit), /* reveal does not reach here, stops in swapd,
                                               * cant verify commit */
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
                                // FIXME should be set before
                                let core_arbitrating_txs = bob.core_arbitrating_transactions(
                                    &params,
                                    bob_params,
                                    funding.clone(),
                                    public_offer,
                                )?;
                                *core_arb_txs = Some(core_arbitrating_txs.clone());
                                let cosign_arbitrating_cancel = bob.cosign_arbitrating_cancel(
                                    core_wallet,
                                    bob_params,
                                    &core_arbitrating_txs,
                                )?;
                                let core_arb_setup = CoreArbitratingSetup::<BtcXmr>::from((
                                    swap_id,
                                    core_arbitrating_txs,
                                    cosign_arbitrating_cancel,
                                ));
                                let core_arb_setup = Msg::CoreArbitratingSetup(core_arb_setup);
                                self.send_ctl(senders, source, Request::Protocol(core_arb_setup))?;
                            }
                            _ => Err(Error::Farcaster("only Some(Wallet::Bob)".to_string()))?,
                        }
                    }
                }
            }
            Request::Protocol(Msg::RefundProcedureSignatures(refund_proc_sigs)) => {
                let swap_id = swap_id(source.clone())?;

                match self.wallets.get_mut(&swap_id) {
                    Some(Wallet::Bob(
                        bob,
                        bob_params,
                        core_wallet,
                        public_offer,
                        Some(_funding_tx),
                        Some(commit),
                        Some(alice_params),
                        Some(core_arbitrating_txs),
                    )) => {
                        // *refund_sigs = Some(refund_proc_sigs);
                        let signed_adaptor_buy = bob.sign_adaptor_buy(
                            core_wallet,
                            alice_params,
                            bob_params,
                            core_arbitrating_txs,
                            public_offer,
                        )?;
                        let signed_arb_lock = bob.sign_arbitrating_lock(
                            core_wallet,
                            core_wallet,
                            core_arbitrating_txs,
                        )?;

                        // TODO: here subscribe to all transactions with syncerd, and publish lock
                        let buy_proc_sig =
                            BuyProcedureSignature::<BtcXmr>::from((swap_id, signed_adaptor_buy));
                        let buy_proc_sig = Msg::BuyProcedureSignature(buy_proc_sig);
                        senders.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            source, // destination swapd
                            Request::Protocol(buy_proc_sig),
                        )?
                    }
                    _ => Err(Error::Farcaster("Unknow wallet and swap_id".to_string()))?,
                }
            }
            Request::Protocol(Msg::CoreArbitratingSetup(core_arb_setup)) => {
                let swap_id = swap_id(source.clone())?;
                let core_arb_txs = core_arb_setup.into();
                match self.wallets.get(&swap_id) {
                    Some(Wallet::Alice(
                        alice,
                        alice_params,
                        core_wallet,
                        public_offer,
                        Some(bob_parameters),
                    )) => {
                        let signed_adaptor_refund = alice.sign_adaptor_refund(
                            core_wallet,
                            alice_params,
                            bob_parameters,
                            &core_arb_txs,
                            public_offer,
                        )?;
                        let cosigned_arb_cancel = alice.cosign_arbitrating_cancel(
                            core_wallet,
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
                        let refund_proc_signatures =
                            Msg::RefundProcedureSignatures(refund_proc_signatures);

                        senders.send_to(
                            ServiceBus::Ctl,
                            self.identity(),
                            source,
                            Request::Protocol(refund_proc_signatures),
                        )?
                    }
                    _ => Err(Error::Farcaster("only Wallet::Alice".to_string()))?,
                }
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
            Request::Hello => match &source {
                ServiceId::Swap(swap_id) => {
                    if let Some(option_req) = self.swaps.get_mut(&swap_id) {
                        trace!("Know swapd, you launched it");
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
                    daemon_service,
                } = public_offer.clone();
                let peer = daemon_service
                    .to_node_addr(LIGHTNING_P2P_DEFAULT_PORT)
                    .ok_or_else(|| internet2::presentation::Error::InvalidEndpoint)?;

                let swap_id: SwapId = SwapId::random().into();
                self.swaps.insert(swap_id, None);
                // since we're takers, we are on the other side
                let taker_role = offer.maker_role.other();
                let core_wallet = CoreWallet::new(self.node_secrets.wallet_seed);
                match taker_role {
                    SwapRole::Bob => {
                        let address = address();
                        let bob: Bob<BtcXmr> = Bob::new(address.into(), FeePolitic::Aggressive);
                        let local_params = bob.generate_parameters(&core_wallet, &public_offer)?;
                        let funding = create_funding()?;
                        info!("Creating {}", "Wallet::Bob".bright_yellow());
                        if self.wallets.get(&swap_id).is_none() {
                            self.wallets.insert(
                                swap_id,
                                Wallet::Bob(
                                    bob,
                                    local_params.clone(),
                                    core_wallet,
                                    public_offer.clone(),
                                    Some(funding),
                                    None,
                                    None,
                                    None,
                                ),
                            );
                        } else {
                            Err(Error::Farcaster(s!("Wallet already exists")))?
                        }
                        let launch_swap = LaunchSwap {
                            peer: peer.into(),
                            local_trade_role: TradeRole::Taker,
                            public_offer,
                            local_params: Params::Bob(local_params),
                            swap_id,
                            remote_commit: None,
                        };
                        senders.send_to(
                            ServiceBus::Ctl,
                            source,
                            ServiceId::Farcasterd,
                            Request::LaunchSwap(launch_swap),
                        )?;
                    }
                    SwapRole::Alice => {
                        let address = address();
                        let alice: Alice<BtcXmr> =
                            Alice::new(address.into(), FeePolitic::Aggressive);
                        let local_params =
                            alice.generate_parameters(&core_wallet, &public_offer)?;
                        let wallet_seed = self.node_secrets.wallet_seed;
                        let core_wallet = CoreWallet::new(wallet_seed);

                        if self.wallets.get(&swap_id).is_none() {
                            // TODO instead of storing in state, start building
                            // requests and store the state in there directly
                            self.wallets.insert(
                                swap_id,
                                Wallet::Alice(
                                    alice,
                                    local_params.clone(),
                                    core_wallet,
                                    public_offer.clone(),
                                    None,
                                ),
                            );
                        } else {
                            Err(Error::Farcaster(s!("Wallet already exists")))?
                        }
                        let launch_swap = LaunchSwap {
                            peer: peer.into(),
                            local_trade_role: TradeRole::Taker,
                            public_offer,
                            local_params: Params::Alice(local_params),
                            swap_id,
                            remote_commit: None,
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
            Request::GetKeys(request::GetKeys(wallet_token, request_id)) => {
                // eprintln!("inside PeerSecret handler");
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
    bitcoin::Address::from_str("bc1qesgvtyx9y6lax0x34napc2m7t5zdq6s7xxwpvk")
        .expect("Parsable address")
}

pub fn create_funding() -> Result<Funding, Error> {
    let tx_hex = Vec::from_hex("01000000000105979237e926bc57c319d98b9c89a929aa404b65ea5a0ab45c9f058377e5ecf5750000000000ffffffffc91bd1d4b3d4fb45aa72fc7d49dd5a3c0a0213f527c58fb5b534ddeb2a9f51130000000000ffffffff5f714adfbad928150c2f22756c024b5282e273be414a74050c9672b146be23000100000000ffffffff2344ae6f3dbee616af09cc039d58fb2826cc46fa995bd48b91e507510c9ed48e0000000000ffffffff29826e5abd7d4b551803a31e07e69c7e7bc72b383b7bf8a95ef62297565d9ed40000000000ffffffff015a17f000000000001600145585e364d7bfe44b0508033c206e4773ad5e8b6d02473044022053a2e2e49c0c8827bf7db07f5f60e2f1320f8fea14fac289e844f221e8f4cd33022036be86e6edabf98d6e6de3581654d51671a3bdee55ed7a969f3c35ca8c9b759001210288bb60d6a8a18df1dc1dd9653251d472133410551f0720caa57286b87192c49302483045022100b47cb32ed0a7764069f688363d1f2619eaa60c73eb929870d02a43c0b818d33102201d2321c14f70d81854377ecc253ad3f935fbf86d0cac7541af254296a3a4180c01210288bb60d6a8a18df1dc1dd9653251d472133410551f0720caa57286b87192c4930247304402201c88d188f4157bec6f8ac4c8b5cb8cf64d0d7af8c338f92a1be5e481221b97340220049d19ac7631b66c127ff660faabc8c70ea41efa4b8ce005ee083f339783f80001210288bb60d6a8a18df1dc1dd9653251d472133410551f0720caa57286b87192c49302483045022100ad73bbba162803c61c5b196d29b05d4e34d4e1524bc37f095f027b4734fcccb602201ee014653f617ca99479be2c506c815d5b5532f252958caa6ba7409a4ed37f2b01210288bb60d6a8a18df1dc1dd9653251d472133410551f0720caa57286b87192c49302483045022100d43abb60f4d4f1d6efc2cd3ab5d8504d54394dec27351b715ad5d307fb90087a022058812c60a378821866208b91ae25eedd4966e371d7af162839308d2a2e98eca901210288bb60d6a8a18df1dc1dd9653251d472133410551f0720caa57286b87192c49300000000").unwrap();
    let funding_tx = Transaction::deserialize(&tx_hex).unwrap();
    let funding_bundle = FundingTransaction::<Bitcoin> {
        funding: funding_tx,
    };

    let mut rng = secp256k1::rand::rngs::OsRng::new().expect("OsRng");
    let (_, pubkey) = (secp256k1::Secp256k1::new()).generate_keypair(&mut rng);
    let pubkey = bitcoin::PublicKey {
        compressed: true,
        key: pubkey,
    };
    let mut funding = Funding::initialize(pubkey, farcaster_core::blockchain::Network::Mainnet)
        .map_err(|_| Error::Farcaster("Impossible to initialize funding tx".to_string()))?;
    funding
        .update(funding_bundle.clone().funding.clone())
        .map_err(|_| Error::Farcaster(s!("Could not update funding")))?;
    Ok(funding)
}
