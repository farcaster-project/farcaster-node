use crate::bus::ctl::{
    BitcoinFundingInfo, CtlMsg, FundingInfo, InitSwap, LaunchSwap, MoneroFundingInfo,
    ProtoPublicOffer, PubOffer, TakerCommitted,
};
use crate::bus::info::{InfoMsg, MadeOffer, OfferInfo, TookOffer};
use crate::bus::p2p::PeerMsg;
use crate::bus::{CheckpointEntry, Failure, FailureCode, OfferStatus, OfferStatusPair};
use crate::farcasterd::runtime::{launch, launch_swapd, syncer_up, Runtime};
use crate::LogStyle;
use crate::{
    bus::{BusMsg, Outcome},
    error::Error,
    event::{Event, StateMachine, StateMachineExecutor},
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
///       |             V            V
///       |       TakerConnect   MakeOffer
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
    /// failure, or TakerConnect if not yet connected. Transition to TakeOffer
    /// triggers a connect peerd launch (if required). If connected, sends
    /// TakeOffer to walletd, sends TookOffer back to cli.
    #[display("Start Taker")]
    StartTaker,

    /// StartRestore - transitions to RestoringSwapd on cli request or None on
    /// failure. Transition to RestoringSwapd triggers launching swapd and
    /// syncers, and peerd connect if we are the taker - sends reply to cli.
    #[display("Start Restore")]
    StartRestore,

    /// TakerConnect state - transitions to TakeOffer once ConnectSuccess is
    /// received from peerd or None if ConnectFailed is received. On
    /// ConnectSuccess sends TakeOffer to walletd and TookOffer back to cli. On
    /// Connect Failed sends Failure back to cli.
    #[display("Taker Connect")]
    TakerConnect(TakerConnect),

    /// MakeOffer state - transitions to TakerCommit once TakerCommit is
    /// received from a counterpary or None if RevokeOffer is received from the
    /// user. Transition to TakerCommit triggers sending TakerCommited to walletd
    /// and sending SetOfferStatus to databased.
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

pub struct TakerConnect {
    public_offer: PublicOffer,
    arb_addr: bitcoin::Address,
    acc_addr: monero::Address,
    peerd: ServiceId,
    source: ServiceId,
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
    trade_role: TradeRole,
    expect_connection: bool,
    connected: bool,
}

pub struct SwapdRunning {
    peerd: Option<ServiceId>,
    public_offer: PublicOffer,
    arbitrating_syncer: ServiceId,
    accordant_syncer: ServiceId,
    swap_id: SwapId,
    funding_info: Option<FundingInfo>,
    auto_funded: bool,
    clients_awaiting_connect_result: Vec<ServiceId>,
    trade_role: TradeRole,
}

impl StateMachine<Runtime, Error> for TradeStateMachine {
    fn next(self, event: Event, runtime: &mut Runtime) -> Result<Option<Self>, Error> {
        debug!(
            "{} | Checking event request {} from {} for state transition",
            self.swap_id().map_or("…".to_string(), |s| s.to_string()),
            event.request,
            event.source
        );
        match self {
            TradeStateMachine::StartTaker => {
                attempt_transition_to_taker_connect_or_take_offer(event, runtime)
            }
            TradeStateMachine::StartMaker => attempt_transition_to_make_offer(event, runtime),
            TradeStateMachine::StartRestore => {
                attempt_transition_to_restoring_swapd(event, runtime)
            }
            TradeStateMachine::TakerConnect(taker_connect) => {
                attempt_transition_to_take_offer(event, runtime, taker_connect)
            }
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

    fn name(&self) -> String {
        let swap_id = self.swap_id().map_or("…".to_string(), |s| s.to_string());
        format!("{} | Trade", swap_id.swap_id())
    }
}

pub struct TradeStateMachineExecutor {}
impl StateMachineExecutor<Runtime, Error, TradeStateMachine> for TradeStateMachineExecutor {}

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
            TradeStateMachine::TakerConnect(TakerConnect { public_offer, .. }) => {
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
            TradeStateMachine::SwapdRunning(SwapdRunning { peerd, .. }) => peerd.clone(),
            _ => None,
        }
    }

    pub fn awaiting_connect_from(&self) -> Option<ServiceId> {
        match self {
            TradeStateMachine::TakerConnect(taker_connect) => Some(taker_connect.peerd.clone()),
            TradeStateMachine::RestoringSwapd(RestoringSwapd {
                public_offer,
                trade_role,
                expect_connection,
                ..
            }) => {
                if *trade_role == TradeRole::Taker && *expect_connection {
                    Some(ServiceId::Peer(node_addr_from_public_offer(&public_offer)))
                } else {
                    None
                }
            }
            TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                public_offer,
                trade_role,
                swap_id,
                ..
            }) => {
                // Peerd is none, implies a connect event is possible. A connect
                // is only possible to a listening peer as a taker, its address
                // thus has to match the address of the offer.
                if *trade_role == TradeRole::Taker && peerd.is_none() {
                    Some(ServiceId::Peer(node_addr_from_public_offer(public_offer)))
                } else if peerd.is_none() {
                    info!(
                        "This should never be triggered, but indeed the peerd of {} is None",
                        swap_id
                    );
                    None
                } else {
                    None
                }
            }
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

fn attempt_transition_to_make_offer(
    mut event: Event,
    runtime: &mut Runtime,
) -> Result<Option<TradeStateMachine>, Error> {
    match event.request.clone() {
        BusMsg::Ctl(CtlMsg::MakeOffer(ProtoPublicOffer {
            offer,
            arbitrating_addr,
            accordant_addr,
            public_addr,
            bind_addr,
            ..
        })) => {
            // start a listener on the bind_addr
            match runtime.listen(bind_addr) {
                Err(err) => {
                    warn!("Failed to start peerd listen, cannot make offer: {}", err);
                    event.complete_client_ctl(CtlMsg::Failure(Failure {
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
                        CtlMsg::SetOfferStatus(OfferStatusPair {
                            offer: public_offer.clone(),
                            status: OfferStatus::Open,
                        }),
                    )?;
                    event.complete_client_info(InfoMsg::MadeOffer(MadeOffer {
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
            warn!(
                "Request {} from {} invalid for state start maker - invalidating.",
                req, event.source
            );
            Ok(None)
        }
    }
}

fn attempt_transition_to_taker_connect_or_take_offer(
    mut event: Event,
    runtime: &mut Runtime,
) -> Result<Option<TradeStateMachine>, Error> {
    match event.request.clone() {
        BusMsg::Ctl(CtlMsg::TakeOffer(PubOffer {
            public_offer,
            external_address,
            internal_address,
        })) => {
            if runtime.consumed_offers_contains(&public_offer)
                || runtime.public_offers.contains(&public_offer)
            {
                let msg = format!(
                    "{} already exists or was already taken, ignoring request",
                    &public_offer.to_string()
                );
                warn!("{}", msg.err());
                event.complete_client_ctl(CtlMsg::Failure(Failure {
                    code: FailureCode::Unknown,
                    info: msg,
                }))?;
                return Ok(None);
            }

            let peer_node_addr = node_addr_from_public_offer(&public_offer);
            // connect to the remote peer
            match runtime.connect_peer(&peer_node_addr) {
                Err(err) => {
                    warn!(
                        "Error connecting to remote peer {}, failed to take offer.",
                        err
                    );
                    event.complete_client_ctl(CtlMsg::Failure(Failure {
                        code: FailureCode::Unknown,
                        info: err.to_string(),
                    }))?;
                    Ok(None)
                }
                Ok((connected, peer_service_id)) => {
                    if connected {
                        let offer_registered = "Public offer registered".to_string();
                        info!(
                            "{}: {:#}",
                            offer_registered.bright_green_bold(),
                            &public_offer.id().bright_yellow_bold()
                        );
                        event.send_ctl_service(
                            ServiceId::Wallet,
                            CtlMsg::TakeOffer(PubOffer {
                                public_offer: public_offer.clone(),
                                external_address: external_address.clone(),
                                internal_address,
                            }),
                        )?;
                        event.complete_client_info(InfoMsg::TookOffer(TookOffer {
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
                    } else {
                        Ok(Some(TradeStateMachine::TakerConnect(TakerConnect {
                            public_offer,
                            arb_addr: external_address,
                            acc_addr: internal_address,
                            peerd: peer_service_id,
                            source: event.source,
                        })))
                    }
                }
            }
        }
        req => {
            warn!(
                "Request {} from {} invalid for state start restore - invalidating.",
                req, event.source,
            );
            Ok(None)
        }
    }
}

fn attempt_transition_to_restoring_swapd(
    mut event: Event,
    runtime: &mut Runtime,
) -> Result<Option<TradeStateMachine>, Error> {
    // check if databased and walletd are running
    match event.request.clone() {
        BusMsg::Ctl(CtlMsg::RestoreCheckpoint(CheckpointEntry {
            swap_id,
            public_offer,
            trade_role,
        })) => {
            if let Err(err) = runtime.services_ready() {
                event.complete_client_ctl(CtlMsg::Failure(Failure {
                    code: FailureCode::Unknown,
                    info: err.to_string(),
                }))?;
                return Ok(None);
            }

            // check if swapd is not running
            if event
                .send_ctl_service(ServiceId::Swap(swap_id), CtlMsg::Hello)
                .is_ok()
            {
                event.complete_client_ctl(CtlMsg::Failure(Failure {
                    code: FailureCode::Unknown,
                    info: "Cannot restore a checkpoint into a running swap.".to_string(),
                }))?;
                return Ok(None);
            }

            // We only try to re-establish a connection if we are the Taker
            let expect_connection = if trade_role == TradeRole::Taker {
                let peer_node_addr = node_addr_from_public_offer(&public_offer);
                if let Err(err) = runtime.connect_peer(&peer_node_addr) {
                    warn!("failed to reconnect to peer on restore: {}", err);
                    false
                } else {
                    true
                }
            } else {
                false
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

            event.complete_client_info(InfoMsg::String("Restoring checkpoint.".to_string()))?;

            Ok(Some(TradeStateMachine::RestoringSwapd(RestoringSwapd {
                public_offer: public_offer.clone(),
                swap_id,
                arbitrating_syncer_up,
                accordant_syncer_up,
                swapd_up: false,
                connected: false,
                expect_connection,
                trade_role,
            })))
        }
        req => {
            warn!(
                "Request {} from {} invalid for state start restore - invalidating.",
                req, event.source,
            );
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
        (BusMsg::P2p(PeerMsg::TakerCommit(taker_commit)), ServiceId::Peer(..)) => {
            if public_offer == taker_commit.public_offer {
                let source = event.source.clone();
                let swap_id = taker_commit.swap_id();
                info!(
                    "Received TakerCommit for swap {} - forwarding to walletd",
                    swap_id,
                );
                event.send_ctl_service(
                    ServiceId::Wallet,
                    CtlMsg::TakerCommitted(TakerCommitted {
                        swap_id,
                        arbitrating_addr: arb_addr,
                        accordant_addr: acc_addr,
                        taker_commit,
                    }),
                )?;
                event.complete_ctl_service(
                    ServiceId::Database,
                    CtlMsg::SetOfferStatus(OfferStatusPair {
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
        (BusMsg::Ctl(CtlMsg::RevokeOffer(revoke_public_offer)), _) => {
            debug!("attempting to revoke {}", public_offer);
            if revoke_public_offer == public_offer {
                info!("Revoked offer {}", public_offer.label());
                event.complete_client_info(InfoMsg::String(
                    "Successfully revoked offer.".to_string(),
                ))?;
                Ok(None)
            } else {
                let msg = "Cannot revoke offer, it does not exist".to_string();
                error!("{}", msg);
                event.complete_client_info(InfoMsg::String(msg))?;
                Ok(Some(TradeStateMachine::MakeOffer(MakeOffer {
                    public_offer,
                    arb_addr,
                    acc_addr,
                })))
            }
        }
        (req, source) => {
            if let BusMsg::Ctl(CtlMsg::Hello) = req {
                trace!(
                    "BusMsg {} from {} invalid for state make offer.",
                    req,
                    source
                );
            } else {
                warn!(
                    "BusMsg {} from {} invalid for state make offer.",
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
    match event.request {
        BusMsg::Ctl(CtlMsg::LaunchSwap(launch_swap)) => {
            let tsm = transition_to_swapd_launched_tsm(runtime, launch_swap, peerd, public_offer)?;
            Ok(Some(tsm))
        }
        req => {
            if let BusMsg::Ctl(CtlMsg::Hello) = req {
                trace!("BusMsg {} from {} invalid for state Taker Commit - expected LaunchSwap request.", req, event.source);
            } else {
                warn!("BusMsg {} from {} invalid for state Taker Commit - expected LaunchSwap request.", req, event.source);
            }
            Ok(Some(TradeStateMachine::TakerCommit(TakerCommit {
                peerd,
                public_offer,
            })))
        }
    }
}

fn attempt_transition_to_take_offer(
    mut event: Event,
    runtime: &mut Runtime,
    taker_connect: TakerConnect,
) -> Result<Option<TradeStateMachine>, Error> {
    let TakerConnect {
        public_offer,
        arb_addr,
        acc_addr,
        peerd,
        source,
    } = taker_connect;
    match event.request {
        BusMsg::Ctl(CtlMsg::ConnectSuccess) if event.source == peerd => {
            runtime.handle_new_connection(event.source.clone());
            let offer_registered = "Public offer registered".to_string();
            info!(
                "{}: {:#}",
                offer_registered.bright_green_bold(),
                &public_offer.id().bright_yellow_bold()
            );
            event.send_ctl_service(
                ServiceId::Wallet,
                CtlMsg::TakeOffer(PubOffer {
                    public_offer: public_offer.clone(),
                    external_address: arb_addr.clone(),
                    internal_address: acc_addr.clone(),
                }),
            )?;
            event.send_client_info(
                source,
                InfoMsg::TookOffer(TookOffer {
                    offerid: public_offer.id(),
                    message: offer_registered,
                }),
            )?;
            runtime.public_offers.insert(public_offer.clone());
            Ok(Some(TradeStateMachine::TakeOffer(TakeOffer {
                public_offer,
                arb_addr,
                acc_addr,
                peerd,
            })))
        }
        BusMsg::Ctl(CtlMsg::ConnectFailed) if event.source == peerd => {
            warn!(
                "{} | Connection to the remote peer {} failed, cannot  take the offer.",
                public_offer.id(),
                peerd
            );
            runtime.handle_failed_connection(event.endpoints, event.source.clone())?;
            event.send_client_ctl(
                source,
                CtlMsg::Failure(Failure {
                    info: format!("Could not connect to remote peer {}.", peerd),
                    code: FailureCode::Unknown,
                }),
            )?;
            Ok(None)
        }
        req => {
            if let BusMsg::Ctl(CtlMsg::Hello) = req {
                trace!(
                    "BusMsg {} from {} invalid for state Take Offer - expected LaunchSwap request.",
                    req,
                    event.source
                );
            } else {
                warn!(
                    "BusMsg {} from {} invalid for state Take Offer - expected LaunchSwap request.",
                    req, event.source
                );
            }
            Ok(Some(TradeStateMachine::TakerConnect(TakerConnect {
                public_offer,
                arb_addr,
                acc_addr,
                peerd,
                source,
            })))
        }
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
    match event.request {
        BusMsg::Ctl(CtlMsg::LaunchSwap(launch_swap)) => {
            let tsm = transition_to_swapd_launched_tsm(runtime, launch_swap, peerd, public_offer)?;
            Ok(Some(tsm))
        }
        req => {
            if let BusMsg::Ctl(CtlMsg::Hello) = req {
                trace!(
                    "BusMsg {} from {} invalid for state Take Offer - expected LaunchSwap request.",
                    req,
                    event.source
                );
            } else {
                warn!(
                    "BusMsg {} from {} invalid for state Take Offer - expected LaunchSwap request.",
                    req, event.source
                );
            }
            Ok(Some(TradeStateMachine::TakeOffer(TakeOffer {
                public_offer,
                arb_addr,
                acc_addr,
                peerd,
            })))
        }
    }
}

fn transition_to_swapd_launched_tsm(
    runtime: &mut Runtime,
    launch_swap: LaunchSwap,
    peerd: ServiceId,
    public_offer: PublicOffer,
) -> Result<TradeStateMachine, Error> {
    let LaunchSwap {
        remote_commit,
        local_params,
        funding_address,
        local_trade_role,
        swap_id,
        ..
    } = launch_swap;
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

    Ok(TradeStateMachine::SwapdLaunched(SwapdLaunched {
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
    }))
}

fn attempt_transition_from_swapd_launched_to_swapd_running(
    event: Event,
    runtime: &mut Runtime,
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
        (BusMsg::Ctl(CtlMsg::Hello), source)
            if ServiceId::Syncer(Blockchain::Monero, public_offer.offer.network) == source =>
        {
            accordant_syncer_up = Some(source);
        }
        (BusMsg::Ctl(CtlMsg::Hello), source) if ServiceId::Swap(swap_id) == source => {
            swapd_up = true;
        }
        (BusMsg::Ctl(CtlMsg::Hello), source)
            if ServiceId::Syncer(Blockchain::Bitcoin, public_offer.offer.network) == source =>
        {
            arbitrating_syncer_up = Some(source);
        }
        (BusMsg::Ctl(CtlMsg::Hello), ServiceId::Peer(..)) => {}
        _ => {
            trace!(
                "{} | BusMsg {} invalid for state swapd launched",
                swap_id,
                event.request
            );
        }
    }
    let peerd_up = runtime.registered_services.contains(&peerd);

    if let (Some(accordant_syncer), Some(arbitrating_syncer), true, true) = (
        accordant_syncer_up.clone(),
        arbitrating_syncer_up.clone(),
        swapd_up,
        peerd_up,
    ) {
        // Tell swapd swap options and link it with the
        // connection daemon
        debug!(
            "{} | swap daemon is known: we spawned it to create a swap. \
                 BusMsging swapd to be the {} of this swap",
            swap_id, local_trade_role,
        );
        let init_swap_req = match local_trade_role {
            TradeRole::Maker => CtlMsg::MakeSwap(init_swap),
            TradeRole::Taker => CtlMsg::TakeSwap(init_swap),
        };
        event.complete_ctl_service(ServiceId::Swap(swap_id), init_swap_req)?;
        Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
            peerd: Some(peerd),
            swap_id,
            accordant_syncer,
            arbitrating_syncer,
            public_offer,
            funding_info: None,
            auto_funded: false,
            clients_awaiting_connect_result: vec![],
            trade_role: local_trade_role,
        })))
    } else {
        debug!(
            "{} | Not transitioning to SwapdRunning yet, service availability: accordant_syncer: {}, arbitrating_syncer: {}, swapd: {}, peerd: {}",
            swap_id,
            accordant_syncer_up.is_some(),
            arbitrating_syncer_up.is_some(),
            swapd_up,
            peerd_up,
        );

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
    mut event: Event,
    runtime: &mut Runtime,
    restoring_swapd: RestoringSwapd,
) -> Result<Option<TradeStateMachine>, Error> {
    let RestoringSwapd {
        public_offer,
        swap_id,
        mut arbitrating_syncer_up,
        mut accordant_syncer_up,
        mut swapd_up,
        mut connected,
        mut expect_connection,
        trade_role,
    } = restoring_swapd;
    match (event.request.clone(), event.source.clone()) {
        (BusMsg::Ctl(CtlMsg::Hello), source)
            if ServiceId::Syncer(Blockchain::Monero, public_offer.offer.network) == source =>
        {
            accordant_syncer_up = Some(source);
        }
        (BusMsg::Ctl(CtlMsg::Hello), source) if ServiceId::Swap(swap_id) == source => {
            swapd_up = true;
        }
        (BusMsg::Ctl(CtlMsg::Hello), source)
            if ServiceId::Syncer(Blockchain::Bitcoin, public_offer.offer.network) == source =>
        {
            arbitrating_syncer_up = Some(source);
        }
        (BusMsg::Ctl(CtlMsg::ConnectSuccess), source)
            if ServiceId::Peer(node_addr_from_public_offer(&public_offer)) == source
                && trade_role == TradeRole::Taker =>
        {
            runtime.handle_new_connection(event.source.clone());

            info!(
                "{} | Peerd connected for restored swap",
                swap_id.bright_blue_italic()
            );
            connected = true;
        }
        (BusMsg::Ctl(CtlMsg::ConnectFailed), source)
            if ServiceId::Peer(node_addr_from_public_offer(&public_offer)) == source
                && trade_role == TradeRole::Taker =>
        {
            runtime.handle_failed_connection(event.endpoints, source.clone())?;
            expect_connection = false;
            connected = false;
        }
        _ => {}
    }
    if let (Some(accordant_syncer), Some(arbitrating_syncer), true, true) = (
        accordant_syncer_up.clone(),
        arbitrating_syncer_up.clone(),
        swapd_up,
        (!expect_connection || expect_connection && connected), // expect_connection implies connected
    ) {
        info!("Restoring swap {}", swap_id.swap_id());
        runtime.stats.incr_initiated();

        let peerd = if connected && trade_role == TradeRole::Taker {
            let peerd = ServiceId::Peer(node_addr_from_public_offer(&public_offer));
            event.send_ctl_service(
                ServiceId::Swap(swap_id),
                CtlMsg::PeerdReconnected(peerd.clone()),
            )?;
            Some(peerd)
        } else {
            None
        };

        event.complete_ctl_service(
            ServiceId::Database,
            CtlMsg::RestoreCheckpoint(CheckpointEntry {
                swap_id,
                public_offer: public_offer.clone(),
                trade_role,
            }),
        )?;

        Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
            peerd,
            swap_id,
            accordant_syncer,
            arbitrating_syncer,
            public_offer,
            auto_funded: false,
            funding_info: None,
            clients_awaiting_connect_result: vec![],
            trade_role,
        })))
    } else {
        Ok(Some(TradeStateMachine::RestoringSwapd(RestoringSwapd {
            public_offer,
            swap_id,
            arbitrating_syncer_up,
            accordant_syncer_up,
            swapd_up,
            connected,
            expect_connection,
            trade_role,
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
        arbitrating_syncer,
        accordant_syncer,
        funding_info,
        auto_funded,
        mut clients_awaiting_connect_result,
        trade_role,
    } = swapd_running;
    match (event.request.clone(), event.source.clone()) {
        (BusMsg::Ctl(CtlMsg::Hello), source)
            if source == peerd.clone().unwrap_or(ServiceId::Loopback) =>
        {
            let swap_service_id = ServiceId::Swap(swap_id);
            debug!("Letting {} know of peer reconnection.", swap_service_id);
            event.complete_ctl_service(swap_service_id, CtlMsg::PeerdReconnected(source))?;
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                public_offer,
                swap_id,
                arbitrating_syncer,
                accordant_syncer,
                funding_info,
                auto_funded,
                clients_awaiting_connect_result,
                trade_role,
            })))
        }

        (BusMsg::Ctl(CtlMsg::FundingInfo(info)), _) => match info {
            FundingInfo::Bitcoin(BitcoinFundingInfo {
                swap_id,
                ref address,
                amount,
            }) => {
                runtime
                    .stats
                    .incr_awaiting_funding(&Blockchain::Bitcoin, swap_id);
                let network = address.network.into();
                if let Some(auto_fund_config) = runtime.config.get_auto_funding_config(network) {
                    info!("{} | Attempting to auto-fund Bitcoin", swap_id.swap_id());
                    debug!("{} | Auto funding config: {:#?}", swap_id, auto_fund_config);

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
                                swap_id.swap_id(),
                                txid.tx_hash()
                            );
                            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                                peerd,
                                public_offer,
                                swap_id,
                                arbitrating_syncer,
                                accordant_syncer,
                                funding_info: Some(info),
                                auto_funded: true,
                                clients_awaiting_connect_result,
                                trade_role,
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
                                arbitrating_syncer,
                                accordant_syncer,
                                funding_info: Some(info),
                                auto_funded: false,
                                clients_awaiting_connect_result,
                                trade_role,
                            })))
                        }
                    }
                } else {
                    Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                        peerd,
                        public_offer,
                        swap_id,
                        arbitrating_syncer,
                        accordant_syncer,
                        funding_info: Some(info.clone()),
                        auto_funded: false,
                        clients_awaiting_connect_result,
                        trade_role,
                    })))
                }
            }
            FundingInfo::Monero(MoneroFundingInfo {
                swap_id,
                address,
                amount,
            }) => {
                runtime
                    .stats
                    .incr_awaiting_funding(&Blockchain::Monero, swap_id);
                let network = address.network.into();
                if let Some(auto_fund_config) = runtime.config.get_auto_funding_config(network) {
                    info!("{} | Attempting to auto-fund Monero", swap_id.swap_id());
                    debug!("{} | Auto funding config: {:#?}", swap_id, auto_fund_config);
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
                                        &swap_id.swap_id(),
                                        tx.tx_hash.tx_hash()
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
                             arbitrating_syncer,
                             accordant_syncer,
                             funding_info: Some(info),
                             auto_funded,
                             clients_awaiting_connect_result,
                             trade_role,
                         })))
                    })
                } else {
                    Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                        peerd,
                        public_offer,
                        swap_id,
                        arbitrating_syncer,
                        accordant_syncer,
                        funding_info: Some(info),
                        auto_funded: false,
                        clients_awaiting_connect_result,
                        trade_role,
                    })))
                }
            }
        },

        (BusMsg::Ctl(CtlMsg::FundingCompleted(blockchain)), _) => {
            runtime.stats.incr_funded(&blockchain, &swap_id);
            info!(
                "{} | Your {} funding completed",
                swap_id.swap_id(),
                blockchain.label()
            );
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                public_offer,
                swap_id,
                arbitrating_syncer,
                accordant_syncer,
                funding_info: None,
                auto_funded,
                clients_awaiting_connect_result,
                trade_role,
            })))
        }

        (BusMsg::Ctl(CtlMsg::FundingCanceled(blockchain)), _) => {
            runtime.stats.incr_funding_canceled(&blockchain, &swap_id);
            info!(
                "{} | Your {} funding was canceled.",
                swap_id.swap_id(),
                blockchain.label()
            );
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                public_offer,
                swap_id,
                arbitrating_syncer,
                accordant_syncer,
                funding_info: None,
                auto_funded,
                clients_awaiting_connect_result,
                trade_role,
            })))
        }

        (BusMsg::Ctl(CtlMsg::Connect(connect_swap_id)), _) if connect_swap_id == swap_id => {
            let mut new_peerd = peerd.clone();
            if let Some(peerd) = peerd {
                event.complete_client_ctl(CtlMsg::Failure(Failure {
                    code: FailureCode::Unknown,
                    info: format!(
                        "The swap already has a dedicated connection daemon {}",
                        peerd
                    ),
                }))?;
            } else {
                let peer_node_addr = node_addr_from_public_offer(&public_offer);
                match runtime.connect_peer(&peer_node_addr) {
                    Err(err) => {
                        event.complete_client_ctl(CtlMsg::Failure(Failure {
                            code: FailureCode::Unknown,
                            info: err.to_string(),
                        }))?;
                    }
                    Ok((connected, peerd)) => {
                        new_peerd = Some(peerd);
                        if connected {
                            event.complete_client_ctl(CtlMsg::ConnectSuccess)?;
                        }
                    }
                }
            }
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd: new_peerd,
                public_offer,
                swap_id,
                arbitrating_syncer,
                accordant_syncer,
                funding_info,
                auto_funded,
                clients_awaiting_connect_result,
                trade_role,
            })))
        }

        // A ConnectSuccess event can only come from a peerd connecting to a listener (maker)
        (BusMsg::Ctl(CtlMsg::ConnectSuccess), source)
            if source.clone() == ServiceId::Peer(node_addr_from_public_offer(&public_offer)) =>
        {
            for client in clients_awaiting_connect_result.drain(..) {
                event.send_client_ctl(client, CtlMsg::ConnectSuccess)?;
            }
            runtime.handle_new_connection(source.clone());
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd: Some(source),
                public_offer,
                swap_id,
                arbitrating_syncer,
                accordant_syncer,
                funding_info,
                auto_funded,
                clients_awaiting_connect_result: vec![],
                trade_role,
            })))
        }

        // A ConnectFailed event can only come from a peerd connecting to a listener (maker)
        (BusMsg::Ctl(CtlMsg::ConnectFailed), source)
            if source.clone() == ServiceId::Peer(node_addr_from_public_offer(&public_offer)) =>
        {
            for client in clients_awaiting_connect_result.drain(..) {
                event.send_client_ctl(
                    client,
                    CtlMsg::Failure(Failure {
                        code: FailureCode::Unknown,
                        info: format!("Failed to connect to remote peer: {}", source),
                    }),
                )?;
            }
            runtime.handle_failed_connection(event.endpoints, source.clone())?;
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd: Some(source),
                public_offer,
                swap_id,
                arbitrating_syncer,
                accordant_syncer,
                funding_info,
                auto_funded,
                clients_awaiting_connect_result: vec![],
                trade_role,
            })))
        }

        (BusMsg::Ctl(CtlMsg::PeerdUnreachable(ServiceId::Peer(addr))), source)
            if ServiceId::Swap(swap_id) == source =>
        {
            if runtime.registered_services.contains(&ServiceId::Peer(addr)) {
                warn!(
                    "Peerd {} was reported to be unreachable, attempting to
                    terminate to kick-off re-connect procedure, if we are
                    taker and the swap is still running.",
                    addr
                );
                event.complete_ctl_service(ServiceId::Peer(addr), CtlMsg::Terminate)?;
            }
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                public_offer,
                swap_id,
                arbitrating_syncer,
                accordant_syncer,
                funding_info,
                auto_funded,
                clients_awaiting_connect_result,
                trade_role,
            })))
        }

        (BusMsg::Ctl(CtlMsg::SwapOutcome(outcome)), source)
            if ServiceId::Swap(swap_id) == source =>
        {
            event.send_ctl_service(
                ServiceId::Database,
                CtlMsg::SetOfferStatus(OfferStatusPair {
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
            if let BusMsg::Ctl(CtlMsg::Hello) = req {
                trace!(
                    "BusMsg {} from {} invalid for state swapd running.",
                    req,
                    source
                );
            } else {
                warn!(
                    "BusMsg {} from {} invalid for state Swapd Running.",
                    req, source
                );
            }
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                public_offer,
                swap_id,
                arbitrating_syncer,
                accordant_syncer,
                funding_info,
                auto_funded,
                clients_awaiting_connect_result,
                trade_role,
            })))
        }
    }
}

fn node_addr_from_public_offer(public_offer: &PublicOffer) -> NodeAddr {
    NodeAddr {
        id: NodeId::from(public_offer.node_id.clone()), // node_id is bitcoin::Pubkey
        addr: public_offer.peer_address,                // peer_address is InetSocketAddr
    }
}
