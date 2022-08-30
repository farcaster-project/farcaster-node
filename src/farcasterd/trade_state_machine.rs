use crate::farcasterd::runtime::{launch, launch_swapd, syncer_up, Runtime};
use crate::rpc::request::{
    BitcoinAddress, BitcoinFundingInfo, CheckpointEntry, FundingInfo, InitSwap, LaunchSwap,
    MadeOffer, MoneroAddress, MoneroFundingInfo, Msg, OfferInfo, OfferStatus, OfferStatusPair,
    ProtoPublicOffer, PubOffer, TakeCommit, TookOffer,
};
use crate::rpc::{Failure, FailureCode};
use crate::LogStyle;
use crate::{
    error::Error,
    event::{Event, StateMachine},
    rpc::request::{Outcome, Request},
    ServiceId,
};
use bitcoin::hashes::hex::ToHex;
use farcaster_core::blockchain::Blockchain;
use farcaster_core::role::TradeRole;
use farcaster_core::swap::{btcxmr::PublicOffer, SwapId};
use internet2::addr::{NodeAddr, NodeId};
use microservices::esb::Handler;
use std::str::FromStr;

/// State machine for launching a swap and cleaning up once done.
///
/// State machine automaton:
/// ```ignore
/// StartRestore    StartTaker  StartMaker
///       |             |            |
///       |             |            V
///       |             |        MakeOffer
///       |             |            |
///       |             V            V
///       |         TakeOffer   TakerCommit
///       |             |____________|
///       |                   |
///       V                   V
/// RestoringSwapd      SwapdLaunched
///       |___________________|
///                 |
///                 V
///            SwapdRunning
///                 |
///                 V
///                End
/// ```
#[derive(Display)]
pub enum TradeStateMachine {
    /// StartMaker state - transitions to MakeOffer on cli request or None on
    /// failure. Transition to MakeOffer triggers a listen peerd launch (if
    /// required), sends SetOfferStatus to databased, sends MadeOffer back to cli.
    #[display("Start Maker")]
    StartMaker,

    /// StartTaker state - transitions to TakeOffer on cli request or None on
    /// failure. Transition to TakeOffer triggers a connect peerd launch (if
    /// required), sends TakeOffer to walletd, sends TookOffer back to cli
    #[display("Start Taker")]
    StartTaker,

    /// StartRestore - transitions to RestoringSwapd on cli request or None on
    /// failure. Transition to RestoringSwapd triggers launching swapd and
    /// syncers, sends reply to cli.
    #[display("Start Restore")]
    StartRestore,

    /// MakeOffer state - transitions to TakerCommit once TakerCommit is
    /// received from a counterpary or None if RevokeOffer is received from the
    /// user. Transition to TakerCommit triggers sending Bitcoin and Monero
    /// addresses to walletd as well as forwarding TakerCommit to walletd and
    /// sending SetOfferStatus to databased.
    #[display("Make Offer")]
    MakeOffer(MakeOffer),

    /// TakerCommit state - transitions to SwapdLaunched once LaunchSwap is
    /// received from walletd. Transition to SwapdLaunched triggers launch
    /// swapd.
    #[display("Taker Commit")]
    TakerCommit(TakerCommit),

    /// TakeOffer state - transitions to SwapdLaunched once LaunchSwap is
    /// received from walletd. Transition to SwapdLaunched triggers launch
    /// swapd.
    #[display("Take Offer")]
    TakeOffer(TakeOffer),

    /// RestoringSwapd state - transitions to SwapdRunning once the various
    /// Hello's are received. Transition to RestoringSwapd triggers
    /// RestoreCheckpoint request to database.
    #[display("Restoring Swapd")]
    RestoringSwapd(RestoringSwapd),

    /// SwapdLaunched state - transitions to SwapRunning once the various
    /// Hello's are reiceved. Transition to SwapRunning triggers sending
    /// TakeSwap to swapd
    #[display("Swapd Launched")]
    SwapdLaunched(SwapdLaunched),

    /// SwapdRunning state - transitions to None once the SwapOutcome is received
    /// from its swapd. Transition to None triggers clean up. Handles requests
    /// affecting the connection status and funding status of a swap.
    #[display("Swapd Running")]
    SwapdRunning(SwapdRunning),
}

pub struct MakeOffer {
    public_offer: PublicOffer,
    arb_addr: bitcoin::Address,
    acc_addr: monero::Address,
}

pub struct TakerCommit {
    peerd: ServiceId,
    public_offer: PublicOffer,
}

pub struct TakeOffer {
    public_offer: PublicOffer,
    arb_addr: bitcoin::Address,
    acc_addr: monero::Address,
    peerd: ServiceId,
}

pub struct SwapdLaunched {
    peerd: ServiceId,
    public_offer: PublicOffer,
    swap_id: SwapId,
    arbitrating_syncer_up: Option<ServiceId>,
    accordant_syncer_up: Option<ServiceId>,
    swapd_up: bool,
    init_swap: InitSwap,
    local_trade_role: TradeRole,
}

pub struct RestoringSwapd {
    public_offer: PublicOffer,
    swap_id: SwapId,
    arbitrating_syncer_up: Option<ServiceId>,
    accordant_syncer_up: Option<ServiceId>,
    swapd_up: bool,
}

pub struct SwapdRunning {
    peerd: ServiceId,
    public_offer: PublicOffer,
    arbitrating_syncer: ServiceId,
    accordant_syncer: ServiceId,
    swap_id: SwapId,
    connected: bool,
    funding_info: Option<FundingInfo>,
    auto_funded: bool,
}

impl StateMachine<Runtime, Error> for TradeStateMachine {
    fn next(self, event: Event, runtime: &mut Runtime) -> Result<Option<Self>, Error> {
        match self {
            TradeStateMachine::StartTaker => transition_to_take_offer(event, runtime),
            TradeStateMachine::StartMaker => transition_to_make_offer(event, runtime),
            TradeStateMachine::StartRestore => transition_to_restoring_swapd(event, runtime),
            TradeStateMachine::MakeOffer(make_offer) => {
                attempt_transition_to_taker_committed(event, runtime, make_offer)
            }
            TradeStateMachine::TakerCommit(taker_commit) => {
                attempt_transition_from_taker_commit_to_swapd_launched(event, runtime, taker_commit)
            }
            TradeStateMachine::TakeOffer(take_offer) => {
                attempt_transition_from_take_offer_to_swapd_launched(event, runtime, take_offer)
            }
            TradeStateMachine::SwapdLaunched(swapd_launched) => {
                attempt_transition_from_swapd_launched_to_swapd_running(
                    event,
                    runtime,
                    swapd_launched,
                )
            }
            TradeStateMachine::RestoringSwapd(restoring_swapd) => {
                attempt_transition_from_restoring_swapd_to_swapd_running(
                    event,
                    runtime,
                    restoring_swapd,
                )
            }
            TradeStateMachine::SwapdRunning(swapd_running) => {
                attempt_transition_to_end(event, runtime, swapd_running)
            }
        }
    }
}

impl TradeStateMachine {
    pub fn open_offer(&self) -> Option<PublicOffer> {
        if let TradeStateMachine::MakeOffer(MakeOffer { public_offer, .. }) = self {
            Some(public_offer.clone())
        } else {
            None
        }
    }

    pub fn consumed_offer(&self) -> Option<PublicOffer> {
        match self {
            TradeStateMachine::TakeOffer(TakeOffer { public_offer, .. }) => {
                Some(public_offer.clone())
            }
            TradeStateMachine::TakerCommit(TakerCommit { public_offer, .. }) => {
                Some(public_offer.clone())
            }
            TradeStateMachine::SwapdLaunched(SwapdLaunched { public_offer, .. }) => {
                Some(public_offer.clone())
            }
            TradeStateMachine::RestoringSwapd(RestoringSwapd { public_offer, .. }) => {
                Some(public_offer.clone())
            }
            TradeStateMachine::SwapdRunning(SwapdRunning { public_offer, .. }) => {
                Some(public_offer.clone())
            }
            _ => None,
        }
    }

    pub fn swap_id(&self) -> Option<SwapId> {
        match self {
            TradeStateMachine::SwapdLaunched(SwapdLaunched { swap_id, .. }) => Some(*swap_id),
            TradeStateMachine::RestoringSwapd(RestoringSwapd { swap_id, .. }) => Some(*swap_id),
            TradeStateMachine::SwapdRunning(SwapdRunning { swap_id, .. }) => Some(*swap_id),
            _ => None,
        }
    }

    pub fn syncers(&self) -> Vec<ServiceId> {
        match self {
            TradeStateMachine::SwapdLaunched(SwapdLaunched {
                arbitrating_syncer_up,
                accordant_syncer_up,
                ..
            }) => {
                let mut syncers = vec![];
                if let Some(syncer) = arbitrating_syncer_up {
                    syncers.push(syncer.clone());
                }
                if let Some(syncer) = accordant_syncer_up {
                    syncers.push(syncer.clone());
                }
                syncers
            }
            TradeStateMachine::SwapdRunning(SwapdRunning {
                arbitrating_syncer,
                accordant_syncer,
                ..
            }) => {
                vec![arbitrating_syncer.clone(), accordant_syncer.clone()]
            }
            _ => {
                vec![]
            }
        }
    }

    pub fn get_connection(&self) -> Option<ServiceId> {
        match self {
            TradeStateMachine::TakeOffer(TakeOffer { peerd, .. }) => Some(peerd.clone()),
            TradeStateMachine::TakerCommit(TakerCommit { peerd, .. }) => Some(peerd.clone()),
            TradeStateMachine::SwapdLaunched(SwapdLaunched { peerd, .. }) => Some(peerd.clone()),
            TradeStateMachine::SwapdRunning(SwapdRunning { peerd, .. }) => Some(peerd.clone()),
            _ => None,
        }
    }

    pub fn needs_funding_monero(&self) -> Option<MoneroFundingInfo> {
        match self {
            TradeStateMachine::SwapdRunning(SwapdRunning {
                funding_info: Some(FundingInfo::Monero(monero_funding_info)),
                auto_funded: false,
                ..
            }) => Some(monero_funding_info.clone()),
            _ => None,
        }
    }

    pub fn needs_funding_bitcoin(&self) -> Option<BitcoinFundingInfo> {
        match self {
            TradeStateMachine::SwapdRunning(SwapdRunning {
                funding_info: Some(FundingInfo::Bitcoin(bitcoin_funding_info)),
                auto_funded: false,
                ..
            }) => Some(bitcoin_funding_info.clone()),
            _ => None,
        }
    }
}

fn transition_to_make_offer(
    mut event: Event,
    runtime: &mut Runtime,
) -> Result<Option<TradeStateMachine>, Error> {
    match event.request.clone() {
        Request::MakeOffer(ProtoPublicOffer {
            offer,
            arbitrating_addr,
            accordant_addr,
            public_addr,
            bind_addr,
            ..
        }) => {
            let node_id = runtime.services_ready().and_then(|_| {
                let (peer_secret_key, peer_public_key) = runtime.peer_keys_ready()?;
                let node_id = NodeId::from(peer_public_key);
                let address_bound = runtime.listens.iter().any(|a| a == &bind_addr);
                if !address_bound {
                    // if address not bound, bind first
                    info!(
                        "{} for incoming peer connections on {}",
                        "Starting listener".bright_blue_bold(),
                        bind_addr.bright_blue_bold()
                    );
                    runtime
                        .listen(NodeAddr::new(node_id, bind_addr), peer_secret_key)
                        .and_then(|_| {
                            runtime.listens.insert(bind_addr);
                            Ok(())
                        })?;
                } else {
                    // no need for the keys, because peerd already knows them
                    let msg = format!("Already listening on {}", &bind_addr);
                    debug!("{}", &msg);
                }
                info!(
                    "Connection daemon {} for incoming peer connections on {}",
                    "listens".bright_green_bold(),
                    bind_addr
                );
                Ok(node_id)
            });
            match node_id {
                Err(err) => {
                    event.complete_ctl(Request::Failure(Failure {
                        code: FailureCode::Unknown,
                        info: err.to_string(),
                    }))?;
                    Ok(None)
                }
                Ok(node_id) => {
                    let public_offer = offer.to_public_v1(node_id.public_key(), public_addr);
                    let msg = s!("Public offer registered, please share with taker.");
                    info!(
                        "{}: {:#}",
                        "Public offer registered.".bright_green_bold(),
                        public_offer.id().bright_yellow_bold()
                    );
                    event.send_ctl_service(
                        ServiceId::Database,
                        Request::SetOfferStatus(OfferStatusPair {
                            offer: public_offer.clone(),
                            status: OfferStatus::Open,
                        }),
                    )?;
                    event.complete_ctl(Request::MadeOffer(MadeOffer {
                        message: msg,
                        offer_info: OfferInfo {
                            offer: public_offer.to_string(),
                            details: public_offer.clone(),
                        },
                    }))?;
                    runtime.public_offers.insert(public_offer.clone());
                    Ok(Some(TradeStateMachine::MakeOffer(MakeOffer {
                        public_offer,
                        arb_addr: arbitrating_addr,
                        acc_addr: accordant_addr,
                    })))
                }
            }
        }
        req => {
            if let Request::Hello = req {
                trace!(
                    "Request {} invalid for state start maker - invalidating.",
                    req
                );
            } else {
                warn!(
                    "Request {} invalid for state start maker - invalidating.",
                    req
                );
            }
            Ok(None)
        }
    }
}

fn transition_to_take_offer(
    mut event: Event,
    runtime: &mut Runtime,
) -> Result<Option<TradeStateMachine>, Error> {
    match event.request.clone() {
        Request::TakeOffer(PubOffer {
            public_offer,
            external_address,
            internal_address,
        }) => {
            if runtime.trade_state_machines.iter().any(|tsm| {
                if let Some(tsm_public_offer) = tsm.consumed_offer() {
                    tsm_public_offer == public_offer
                } else {
                    false
                }
            }) || runtime.public_offers.contains(&public_offer)
            {
                let msg = format!(
                    "{} already exists or was already taken, ignoring request",
                    &public_offer.to_string()
                );
                warn!("{}", msg.err());
                event.complete_ctl(Request::Failure(Failure {
                    code: FailureCode::Unknown,
                    info: msg,
                }))?;
                return Ok(None);
            }
            let PublicOffer {
                version: _,
                offer: _,
                node_id,      // bitcoin::Pubkey
                peer_address, // InetSocketAddr
            } = public_offer;

            let peer = internet2::addr::NodeAddr {
                id: NodeId::from(node_id.clone()), // checked above
                addr: peer_address,
            };
            let res = runtime.services_ready().and_then(|_| {
                let (peer_secret_key, _) = runtime.peer_keys_ready()?;
                // Connect
                if let Some(existing_peer) = runtime.registered_services.iter().find(|service| {
                    if let ServiceId::Peer(node_addr) = service {
                        node_addr.id.public_key() == node_id
                    } else {
                        false
                    }
                }) {
                    warn!(
                        "Already connected to remote peer {} through a listener spawned connection {}",
                        node_id, existing_peer
                    );
                    Ok(existing_peer.clone())
                } else if runtime.registered_services.contains(&ServiceId::Peer(peer)) {
                    warn!(
                        "Already connected to remote peer {}",
                        peer.bright_blue_italic()
                    );
                    Ok(ServiceId::Peer(peer))
                } else {
                    debug!(
                        "{} to remote peer {}",
                        "Connecting".bright_blue_bold(),
                        peer.bright_blue_italic()
                    );
                    runtime.connect_peer(&peer, peer_secret_key)?;
                    Ok(ServiceId::Peer(peer))
                }
            });
            match res {
                Err(err) => {
                    event.complete_ctl(Request::Failure(Failure {
                        code: FailureCode::Unknown,
                        info: err.to_string(),
                    }))?;
                    Ok(None)
                }
                Ok(peer_service_id) => {
                    let offer_registered = "Public offer registered".to_string();
                    info!(
                        "{}: {:#}",
                        offer_registered.bright_green_bold(),
                        &public_offer.id().bright_yellow_bold()
                    );

                    event.send_ctl_service(
                        ServiceId::Wallet,
                        Request::TakeOffer(PubOffer {
                            public_offer: public_offer.clone(),
                            external_address: external_address.clone(),
                            internal_address,
                        }),
                    )?;
                    event.complete_ctl(Request::TookOffer(TookOffer {
                        offerid: public_offer.id(),
                        message: offer_registered,
                    }))?;
                    runtime.public_offers.insert(public_offer.clone());
                    Ok(Some(TradeStateMachine::TakeOffer(TakeOffer {
                        public_offer,
                        arb_addr: external_address,
                        acc_addr: internal_address,
                        peerd: peer_service_id,
                    })))
                }
            }
        }
        req => {
            if let Request::Hello = req {
                trace!(
                    "Request {} invalid for state start restore - invalidating.",
                    req
                );
            } else {
                warn!(
                    "Request {} invalid for state start restore - invalidating.",
                    req
                );
            }
            Ok(None)
        }
    }
}

fn transition_to_restoring_swapd(
    mut event: Event,
    runtime: &mut Runtime,
) -> Result<Option<TradeStateMachine>, Error> {
    // check if databased and walletd are running
    match event.request.clone() {
        Request::RestoreCheckpoint(swap_id) => {
            if let Err(err) = runtime.services_ready() {
                event.send_ctl_service(
                    event.source.clone(),
                    Request::Failure(Failure {
                        code: FailureCode::Unknown,
                        info: err.to_string(),
                    }),
                )?;
                return Ok(None);
            }

            // check if swapd is not running
            if event
                .send_ctl_service(ServiceId::Swap(swap_id), Request::Hello)
                .is_ok()
            {
                event.complete_ctl(Request::Failure(Failure {
                    code: FailureCode::Unknown,
                    info: "Cannot restore a checkpoint into a running swap.".to_string(),
                }))?;
                return Ok(None);
            }

            let CheckpointEntry {
                public_offer,
                trade_role,
                ..
            } = match runtime
                .checkpointed_pub_offers
                .iter()
                .find(|entry| entry.swap_id == swap_id)
            {
                Some(ce) => ce,
                None => {
                    event.complete_ctl(Request::Failure(Failure {
                        code: FailureCode::Unknown,
                        info: "No checkpoint found with given swap id, aborting restore."
                            .to_string(),
                    }))?;
                    return Ok(None);
                }
            };
            let arbitrating_syncer_up = syncer_up(
                &mut runtime.spawning_services,
                &mut runtime.registered_services,
                Blockchain::Bitcoin,
                public_offer.offer.network,
                &runtime.config,
            )?;
            let accordant_syncer_up = syncer_up(
                &mut runtime.spawning_services,
                &mut runtime.registered_services,
                Blockchain::Monero,
                public_offer.offer.network,
                &runtime.config,
            )?;

            let _child = launch(
                "swapd",
                &[
                    swap_id.to_hex(),
                    public_offer.to_string(),
                    trade_role.to_string(),
                ],
            )?;

            event.complete_ctl(Request::String("Restoring checkpoint.".to_string()))?;

            Ok(Some(TradeStateMachine::RestoringSwapd(RestoringSwapd {
                public_offer: public_offer.clone(),
                swap_id,
                arbitrating_syncer_up,
                accordant_syncer_up,
                swapd_up: false,
            })))
        }
        req => {
            if let Request::Hello = req {
                trace!(
                    "Request {} invalid for state start restore - invalidating.",
                    req
                );
            } else {
                warn!(
                    "Request {} invalid for state start restore - invalidating.",
                    req
                );
            }
            Ok(None)
        }
    }
}

fn attempt_transition_to_taker_committed(
    mut event: Event,
    _runtime: &mut Runtime,
    make_offer: MakeOffer,
) -> Result<Option<TradeStateMachine>, Error> {
    let MakeOffer {
        public_offer,
        arb_addr,
        acc_addr,
    } = make_offer;
    match (event.request.clone(), event.source.clone()) {
        (
            Request::Protocol(Msg::TakerCommit(TakeCommit {
                public_offer: committed_public_offer,
                swap_id,
                ..
            })),
            ServiceId::Peer(..),
        ) => {
            if public_offer == committed_public_offer {
                let source = event.source.clone();
                let btc_addr_req = Request::BitcoinAddress(BitcoinAddress(swap_id, arb_addr));
                event.send_msg_service(ServiceId::Wallet, btc_addr_req)?;
                let xmr_addr_req = Request::MoneroAddress(MoneroAddress(swap_id, acc_addr));
                event.send_msg_service(ServiceId::Wallet, xmr_addr_req)?;
                info!("passing request to walletd from {}", event.source);
                event.forward_msg(ServiceId::Wallet)?;
                event.complete_ctl_service(
                    ServiceId::Database,
                    Request::SetOfferStatus(OfferStatusPair {
                        offer: public_offer.clone(),
                        status: OfferStatus::InProgress,
                    }),
                )?;
                Ok(Some(TradeStateMachine::TakerCommit(TakerCommit {
                    peerd: source,
                    public_offer,
                })))
            } else {
                error!(
                    "Received invalid TakerCommit for offer {}.",
                    public_offer.offer.id()
                );
                Ok(Some(TradeStateMachine::MakeOffer(MakeOffer {
                    public_offer,
                    arb_addr,
                    acc_addr,
                })))
            }
        }
        (Request::RevokeOffer(revoke_public_offer), _) => {
            debug!("attempting to revoke {}", public_offer);
            if revoke_public_offer == public_offer {
                info!("Revoked offer {}", public_offer);
                event.complete_ctl(Request::String("Successfully revoked offer.".to_string()))?;
                Ok(None)
            } else {
                let msg = "Cannot revoke offer, it does not exist".to_string();
                error!("{}", msg);
                event.complete_ctl(Request::String(msg))?;
                Ok(Some(TradeStateMachine::MakeOffer(MakeOffer {
                    public_offer,
                    arb_addr,
                    acc_addr,
                })))
            }
        }
        (req, source) => {
            if let Request::Hello = req {
                trace!(
                    "Request {} from {} invalid for state make offer.",
                    req,
                    source
                );
            } else {
                warn!(
                    "Request {} from {} invalid for state make offer.",
                    req, source
                );
            }
            Ok(Some(TradeStateMachine::MakeOffer(MakeOffer {
                public_offer,
                arb_addr,
                acc_addr,
            })))
        }
    }
}

fn attempt_transition_from_taker_commit_to_swapd_launched(
    event: Event,
    runtime: &mut Runtime,
    taker_commit: TakerCommit,
) -> Result<Option<TradeStateMachine>, Error> {
    let TakerCommit {
        peerd,
        public_offer,
    } = taker_commit;
    if let Some(tsm) = attempt_transition_to_swapd_launched(event, runtime, &peerd, &public_offer)?
    {
        Ok(Some(tsm))
    } else {
        Ok(Some(TradeStateMachine::TakerCommit(TakerCommit {
            peerd,
            public_offer,
        })))
    }
}

fn attempt_transition_from_take_offer_to_swapd_launched(
    event: Event,
    runtime: &mut Runtime,
    take_offer: TakeOffer,
) -> Result<Option<TradeStateMachine>, Error> {
    let TakeOffer {
        public_offer,
        arb_addr,
        acc_addr,
        peerd,
    } = take_offer;
    if let Some(tsm) = attempt_transition_to_swapd_launched(event, runtime, &peerd, &public_offer)?
    {
        Ok(Some(tsm))
    } else {
        Ok(Some(TradeStateMachine::TakeOffer(TakeOffer {
            public_offer,
            arb_addr,
            acc_addr,
            peerd,
        })))
    }
}

fn attempt_transition_to_swapd_launched(
    event: Event,
    runtime: &mut Runtime,
    peerd: &ServiceId,
    public_offer: &PublicOffer,
) -> Result<Option<TradeStateMachine>, Error> {
    match event.request {
        Request::LaunchSwap(LaunchSwap {
            remote_commit,
            local_params,
            funding_address,
            local_trade_role,
            swap_id,
            ..
        }) => {
            let network = public_offer.offer.network;
            let arbitrating_syncer_up = syncer_up(
                &mut runtime.spawning_services,
                &mut runtime.registered_services,
                Blockchain::Bitcoin,
                network,
                &runtime.config,
            )?;
            let accordant_syncer_up = syncer_up(
                &mut runtime.spawning_services,
                &mut runtime.registered_services,
                Blockchain::Monero,
                network,
                &runtime.config,
            )?;
            trace!(
                "launching swapd with swap_id: {}",
                swap_id.bright_yellow_bold()
            );

            runtime.stats.incr_initiated();
            launch_swapd(local_trade_role, public_offer.clone(), swap_id)?;

            Ok(Some(TradeStateMachine::SwapdLaunched(SwapdLaunched {
                peerd: peerd.clone(),
                swap_id,
                public_offer: public_offer.clone(),
                arbitrating_syncer_up,
                accordant_syncer_up,
                swapd_up: false,
                init_swap: InitSwap {
                    peerd: peerd.clone(),
                    report_to: Some(runtime.identity()),
                    local_params,
                    swap_id,
                    remote_commit,
                    funding_address,
                },
                local_trade_role,
            })))
        }
        req => {
            if let Request::Hello = req {
                trace!("Request {} - expected LaunchSwap request.", req);
            } else {
                warn!("Request {} - expected LaunchSwap request.", req);
            }
            Ok(None)
        }
    }
}

fn attempt_transition_from_swapd_launched_to_swapd_running(
    event: Event,
    _runtime: &mut Runtime,
    swapd_launched: SwapdLaunched,
) -> Result<Option<TradeStateMachine>, Error> {
    let SwapdLaunched {
        peerd,
        public_offer,
        swap_id,
        mut arbitrating_syncer_up,
        mut accordant_syncer_up,
        mut swapd_up,
        init_swap,
        local_trade_role,
    } = swapd_launched;
    match (event.request.clone(), event.source.clone()) {
        (Request::Hello, source)
            if ServiceId::Syncer(Blockchain::Monero, public_offer.offer.network) == source =>
        {
            accordant_syncer_up = Some(source);
        }
        (Request::Hello, source) if ServiceId::Swap(swap_id) == source => {
            swapd_up = true;
        }
        (Request::Hello, source)
            if ServiceId::Syncer(Blockchain::Bitcoin, public_offer.offer.network) == source =>
        {
            arbitrating_syncer_up = Some(source);
        }
        _ => {
            trace!("Request {} invalid for state swapd launched", event.request);
        }
    }
    if let (Some(accordant_syncer), Some(arbitrating_syncer), true) = (
        accordant_syncer_up.clone(),
        arbitrating_syncer_up.clone(),
        swapd_up,
    ) {
        // Tell swapd swap options and link it with the
        // connection daemon
        debug!(
            "Swapd {} is known: we spawned it to create a swap. \
                 Requesting swapd to be the {} of this swap",
            swap_id, local_trade_role,
        );
        let init_swap_req = match local_trade_role {
            TradeRole::Maker => Request::MakeSwap(init_swap),
            TradeRole::Taker => Request::TakeSwap(init_swap),
        };
        event.complete_ctl_service(ServiceId::Swap(swap_id), init_swap_req)?;
        Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
            peerd,
            swap_id,
            accordant_syncer,
            arbitrating_syncer,
            public_offer,
            connected: true,
            funding_info: None,
            auto_funded: false,
        })))
    } else {
        Ok(Some(TradeStateMachine::SwapdLaunched(SwapdLaunched {
            swap_id,
            public_offer,
            peerd,
            arbitrating_syncer_up,
            accordant_syncer_up,
            swapd_up,
            init_swap,
            local_trade_role,
        })))
    }
}

fn attempt_transition_from_restoring_swapd_to_swapd_running(
    event: Event,
    runtime: &mut Runtime,
    restoring_swapd: RestoringSwapd,
) -> Result<Option<TradeStateMachine>, Error> {
    let RestoringSwapd {
        public_offer,
        swap_id,
        mut arbitrating_syncer_up,
        mut accordant_syncer_up,
        mut swapd_up,
    } = restoring_swapd;
    match (event.request.clone(), event.source.clone()) {
        (Request::Hello, source)
            if ServiceId::Syncer(Blockchain::Monero, public_offer.offer.network) == source =>
        {
            accordant_syncer_up = Some(source);
        }
        (Request::Hello, source) if ServiceId::Swap(swap_id) == source => {
            swapd_up = true;
        }
        (Request::Hello, source)
            if ServiceId::Syncer(Blockchain::Bitcoin, public_offer.offer.network) == source =>
        {
            arbitrating_syncer_up = Some(source);
        }
        _ => {}
    }
    if let (Some(accordant_syncer), Some(arbitrating_syncer), true) = (
        accordant_syncer_up.clone(),
        arbitrating_syncer_up.clone(),
        swapd_up,
    ) {
        info!("Restoring swap {}", swap_id.bright_blue_italic());
        runtime.stats.incr_initiated();
        event.complete_ctl_service(ServiceId::Database, Request::RestoreCheckpoint(swap_id))?;

        Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
            peerd: ServiceId::Loopback, // TODO: Move this from the dummy value to the actual peerd once we handle reconnect
            swap_id,
            accordant_syncer,
            arbitrating_syncer,
            public_offer,
            connected: true,
            auto_funded: false,
            funding_info: None,
        })))
    } else {
        Ok(Some(TradeStateMachine::RestoringSwapd(RestoringSwapd {
            public_offer,
            swap_id,
            arbitrating_syncer_up,
            accordant_syncer_up,
            swapd_up,
        })))
    }
}

fn attempt_transition_to_end(
    mut event: Event,
    runtime: &mut Runtime,
    swapd_running: SwapdRunning,
) -> Result<Option<TradeStateMachine>, Error> {
    let SwapdRunning {
        peerd,
        public_offer,
        swap_id,
        connected,
        arbitrating_syncer,
        accordant_syncer,
        funding_info,
        auto_funded,
    } = swapd_running;
    match (event.request.clone(), event.source.clone()) {
        (Request::Hello, source) if source == peerd => {
            let swap_service_id = ServiceId::Swap(swap_id);
            debug!("Letting {} know of peer reconnection.", swap_service_id);
            event.complete_ctl_service(swap_service_id, Request::PeerdReconnected(source))?;
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                public_offer,
                swap_id,
                connected: true,
                arbitrating_syncer,
                accordant_syncer,
                funding_info,
                auto_funded,
            })))
        }

        (Request::FundingInfo(info), _) => match info {
            FundingInfo::Bitcoin(BitcoinFundingInfo {
                swap_id,
                ref address,
                amount,
            }) => {
                runtime.stats.incr_awaiting_funding(&Blockchain::Bitcoin);
                let network = address.network.into();
                if let Some(auto_fund_config) = runtime.config.get_auto_funding_config(network) {
                    info!(
                        "{} | Attempting to auto-fund Bitcoin",
                        swap_id.bright_blue_italic()
                    );
                    debug!(
                        "{} | Auto funding config: {:#?}",
                        swap_id.bright_blue_italic(),
                        auto_fund_config
                    );

                    use bitcoincore_rpc::{Auth, Client, Error, RpcApi};
                    use std::path::PathBuf;

                    let host = auto_fund_config.bitcoin_rpc;
                    let bitcoin_rpc = match auto_fund_config.bitcoin_cookie_path {
                            Some(cookie) => {
                                let path = PathBuf::from_str(&shellexpand::tilde(&cookie)).unwrap();
                                debug!("{} | bitcoin-rpc connecting with cookie auth",
                                       swap_id.bright_blue_italic());
                                Client::new(&host, Auth::CookieFile(path))
                            }
                            None => {
                                match (auto_fund_config.bitcoin_rpc_user, auto_fund_config.bitcoin_rpc_pass) {
                                    (Some(rpc_user), Some(rpc_pass)) => {
                                        debug!("{} | bitcoin-rpc connecting with userpass auth",
                                               swap_id.bright_blue_italic());
                                        Client::new(&host, Auth::UserPass(rpc_user, rpc_pass))
                                    }
                                    _ => {
                                        error!(
                                            "{} | Couldn't instantiate Bitcoin RPC - provide either `bitcoin_cookie_path` or `bitcoin_rpc_user` AND `bitcoin_rpc_pass` configuration parameters",
                                            swap_id.bright_blue_italic()
                                        );

                                        Err(Error::InvalidCookieFile)}
                                }
                            }
                        }.unwrap();

                    match bitcoin_rpc
                        .send_to_address(&address, amount, None, None, None, None, None, None)
                    {
                        Ok(txid) => {
                            info!(
                                "{} | Auto-funded Bitcoin with txid: {}",
                                swap_id.bright_blue_italic(),
                                txid
                            );
                            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                                peerd,
                                public_offer,
                                swap_id,
                                connected: true,
                                arbitrating_syncer,
                                accordant_syncer,
                                funding_info: Some(info),
                                auto_funded: true,
                            })))
                        }
                        Err(err) => {
                            warn!("{}", err);
                            error!(
                                    "{} | Auto-funding Bitcoin transaction failed, pushing to cli, use `swap-cli needs-funding Bitcoin` to retrieve address and amount",
                                    swap_id.bright_blue_italic()
                                );
                            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                                peerd,
                                public_offer,
                                swap_id,
                                connected: true,
                                arbitrating_syncer,
                                accordant_syncer,
                                funding_info: Some(info),
                                auto_funded: false,
                            })))
                        }
                    }
                } else {
                    Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                        peerd,
                        public_offer,
                        swap_id,
                        connected: true,
                        arbitrating_syncer,
                        accordant_syncer,
                        funding_info: Some(info.clone()),
                        auto_funded: false,
                    })))
                }
            }
            FundingInfo::Monero(MoneroFundingInfo {
                swap_id,
                address,
                amount,
            }) => {
                runtime.stats.incr_awaiting_funding(&Blockchain::Monero);
                let network = address.network.into();
                if let Some(auto_fund_config) = runtime.config.get_auto_funding_config(network) {
                    info!(
                        "{} | Attempting to auto-fund Monero",
                        swap_id.bright_blue_italic()
                    );
                    debug!(
                        "{} | Auto funding config: {:#?}",
                        swap_id.bright_blue_italic(),
                        auto_fund_config
                    );
                    use tokio::runtime::Builder;
                    let rt = Builder::new_multi_thread()
                        .worker_threads(1)
                        .enable_all()
                        .build()
                        .unwrap();
                    rt.block_on(async {
                        let host = auto_fund_config.monero_rpc_wallet;
                        let wallet_client =
                            monero_rpc::RpcClient::new(host);
                        let wallet = wallet_client.wallet();
                        let options = monero_rpc::TransferOptions::default();

                        let mut auto_funded = false;
                        for retries in (0..10).rev() {
                            match wallet
                                .transfer(
                                    [(address, amount)].iter().cloned().collect(),
                                    monero_rpc::TransferPriority::Default,
                                    options.clone(),
                                )
                                .await
                            {
                                Ok(tx) => {
                                    info!(
                                        "{} | Auto-funded Monero with txid: {}",
                                        &swap_id.bright_blue_italic(),
                                        tx.tx_hash.to_string()
                                    );
                                    auto_funded = true;
                                    break;
                                }
                                Err(err) => {
                                    if (err.to_string().contains("not enough") && err.to_string().contains("money")) || retries == 0 {
                                        warn!("{}", err);
                                        error!("{} | Auto-funding Monero transaction failed, pushing to cli, use `swap-cli needs-funding Monero` to retrieve address and amount", &swap_id.bright_blue_italic());
                                        break;
                                    } else {
                                        warn!("{} | Auto-funding Monero transaction failed with {}, retrying, {} retries left", &swap_id.bright_blue_italic(), err, retries);
                                    }
                                }
                            }
                        }
                        Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                             peerd,
                             public_offer,
                             swap_id,
                             connected: true,
                             arbitrating_syncer,
                             accordant_syncer,
                             funding_info: Some(info),
                             auto_funded,
                         })))
                    })
                } else {
                    Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                        peerd,
                        public_offer,
                        swap_id,
                        connected: true,
                        arbitrating_syncer,
                        accordant_syncer,
                        funding_info: Some(info),
                        auto_funded: false,
                    })))
                }
            }
        },

        (Request::FundingCompleted(blockchain), _) => {
            runtime.stats.incr_funded(&blockchain);
            info!(
                "{} | Your {} funding completed",
                swap_id.bright_blue_italic(),
                blockchain.bright_green_bold()
            );
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                public_offer,
                swap_id,
                connected: true,
                arbitrating_syncer,
                accordant_syncer,
                funding_info: None,
                auto_funded,
            })))
        }

        (Request::FundingCanceled(blockchain), _) => {
            match blockchain {
                Blockchain::Bitcoin => {
                    runtime.stats.incr_funding_bitcoin_canceled();
                    info!(
                        "{} | Your {} funding was canceled.",
                        swap_id.bright_blue_italic(),
                        blockchain.bright_green_bold()
                    );
                }
                Blockchain::Monero => {
                    runtime.stats.incr_funding_monero_canceled();
                    info!(
                        "{} | Your {} funding was canceled.",
                        swap_id.bright_blue_italic(),
                        blockchain.bright_green_bold()
                    );
                }
            };
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                public_offer,
                swap_id,
                connected: true,
                arbitrating_syncer,
                accordant_syncer,
                funding_info: None,
                auto_funded,
            })))
        }

        (Request::PeerdUnreachable(ServiceId::Peer(addr)), source)
            if ServiceId::Swap(swap_id) == source =>
        {
            if runtime.registered_services.contains(&ServiceId::Peer(addr)) {
                warn!(
                    "Peerd {} was reported to be unreachable, attempting to
                    terminate to kick-off re-connect procedure, if we are
                    taker and the swap is still running.",
                    addr
                );
                event.complete_ctl_service(ServiceId::Peer(addr), Request::Terminate)?;
            }
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                public_offer,
                swap_id,
                connected: false,
                arbitrating_syncer,
                accordant_syncer,
                funding_info,
                auto_funded,
            })))
        }

        (Request::SwapOutcome(outcome), source) if ServiceId::Swap(swap_id) == source => {
            event.send_ctl_service(
                ServiceId::Database,
                Request::SetOfferStatus(OfferStatusPair {
                    offer: public_offer,
                    status: OfferStatus::Ended(outcome.clone()),
                }),
            )?;
            runtime.clean_up_after_swap(&swap_id, event.endpoints)?;
            runtime.stats.incr_outcome(&outcome);
            match outcome {
                Outcome::Buy => {
                    debug!("Success on swap {}", swap_id);
                }
                Outcome::Refund => {
                    warn!("Refund on swap {}", swap_id);
                }
                Outcome::Punish => {
                    warn!("Punish on swap {}", swap_id);
                }
                Outcome::Abort => {
                    warn!("Aborted swap {}", swap_id);
                }
            }
            runtime.stats.success_rate();
            Ok(None)
        }

        (req, source) => {
            if let Request::Hello = req {
                trace!(
                    "Request {} from {} invalid for state swapd running.",
                    req,
                    source
                );
            } else {
                warn!(
                    "Request {} from {} invalid for state Swapd Running.",
                    req, source
                );
            }
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                public_offer,
                swap_id,
                connected,
                arbitrating_syncer,
                accordant_syncer,
                funding_info,
                auto_funded,
            })))
        }
    }
}
