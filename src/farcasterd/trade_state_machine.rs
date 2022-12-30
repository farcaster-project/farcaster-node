// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use crate::bus::ctl::{
    BitcoinFundingInfo, CtlMsg, FundingInfo, InitMakerSwap, InitTakerSwap, MoneroFundingInfo,
    ProtoDeal, PubDeal, SwapKeys, WrappedKeyManager,
};
use crate::bus::info::{InfoMsg, MadeDeal, TookDeal, ViewableDeal};
use crate::bus::p2p::{Commit, PeerMsg};
use crate::bus::{CheckpointEntry, DealInfo, DealStatus, Failure, FailureCode};
use crate::farcasterd::runtime::{launch_swapd, syncer_up, Runtime};
use crate::service::{SwapDetails, SwapLogging};
use crate::LogStyle;
use crate::{
    bus::{BusMsg, Outcome},
    error::Error,
    event::{Event, StateMachine, StateMachineExecutor},
    ServiceId,
};
use farcaster_core::blockchain::Blockchain;
use farcaster_core::role::{SwapRole, TradeRole};
use farcaster_core::swap::{btcxmr::Deal, SwapId};
use farcaster_core::Uuid;
use internet2::addr::{NodeAddr, NodeId};
use microservices::esb::Handler;
use std::convert::TryInto;
use std::str::FromStr;

/// State machine for launching a swap and cleaning up once done.
///
/// State machine automaton:
/// ```ignore
/// StartRestore   StartMaker    StartTaker
///       |            |             |
///       |            |         ____|____
///       |            |        |         |
///       |            V        |         V
///       |        MakeDeal     |   TakerConnect
///       |            |        |         |
///       |            V        V         V
///       |        TakerCommit   TakeDeal
///       |            |____________|
///       |                  |
///       V                  V
/// RestoringSwapd     SwapdLaunched
///       |__________________|
///                |
///                V
///           SwapdRunning
///                |
///                V
///               End
/// ```
#[derive(Display)]
pub enum TradeStateMachine {
    /// StartMaker state - transitions to MakeDeal on cli request or None on
    /// failure. Transition to MakeDeal triggers a listen peerd launch (if
    /// required), sends SetDealStatus to databased, sends MadeDeal back to cli.
    #[display("Start Maker")]
    StartMaker,

    /// StartTaker state - transitions to TakeDeal on cli request or None on
    /// failure, or TakerConnect if not yet connected. Transition to TakeDeal
    /// triggers a connect peerd launch (if required). If connected, sends
    /// CreateSwapKeys to walletd, sends TookDeal back to cli.
    #[display("Start Taker")]
    StartTaker,

    /// StartRestore - transitions to RestoringSwapd on cli request or None on
    /// failure. Transition to RestoringSwapd triggers launching swapd and
    /// syncers, and peerd connect if we are the taker - sends reply to cli.
    #[display("Start Restore")]
    StartRestore,

    /// TakerConnect state - transitions to TakeDeal once ConnectSuccess is
    /// received from peerd or None if ConnectFailed is received. On
    /// ConnectSuccess sends CreateSwapKeys to walletd and TookDeal back to
    /// cli. On Connect Failed sends Failure back to cli.
    #[display("Taker Connect")]
    TakerConnect(TakerConnect),

    /// MakeDeal state - transitions to TakerCommit once TakerCommit is
    /// received from a counterpary or None if RevokeDeal is received from the
    /// user. Transition to TakerCommit triggers sending CreateSwapKeys to
    /// walletd and sending SetDealStatus to databased.
    #[display("Make Deal")]
    MakeDeal(MakeDeal),

    /// TakerCommit state - transitions to SwapdLaunched once SwapKeys is
    /// received from walletd. Transition to SwapdLaunched triggers launch
    /// swapd.
    #[display("Taker Commit")]
    TakerCommit(TakerCommit),

    /// TakeDeal state - transitions to SwapdLaunched once SwapKeys is
    /// received from walletd. Transition to SwapdLaunched triggers launch
    /// swapd.
    #[display("Take Deal")]
    TakeDeal(TakeDeal),

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

pub struct MakeDeal {
    deal: Deal,
    arb_addr: bitcoin::Address,
    acc_addr: monero::Address,
}

pub struct TakerCommit {
    peerd: ServiceId,
    deal: Deal,
    commit: Commit,
    target_bitcoin_address: bitcoin::Address,
    target_monero_address: monero::Address,
}

pub struct TakerConnect {
    deal: Deal,
    arb_addr: bitcoin::Address,
    acc_addr: monero::Address,
    source: ServiceId,
}

pub struct TakeDeal {
    deal: Deal,
    arb_addr: bitcoin::Address,
    acc_addr: monero::Address,
    peerd: ServiceId,
}

pub struct SwapdLaunched {
    peerd: ServiceId,
    deal: Deal,
    swap_id: SwapId,
    arbitrating_syncer_up: Option<ServiceId>,
    accordant_syncer_up: Option<ServiceId>,
    swapd_up: bool,
    consumed_deal_role: ConsumedDealRole,
    peerd_reconnected: bool,
    key_manager: WrappedKeyManager,
    target_bitcoin_address: bitcoin::Address,
    target_monero_address: monero::Address,
}

pub struct RestoringSwapd {
    swap_id: SwapId,
    deal: Deal,
    trade_role: TradeRole,
    expected_counterparty_node_id: Option<NodeId>,
    arbitrating_syncer_up: Option<ServiceId>,
    accordant_syncer_up: Option<ServiceId>,
    swapd_up: bool,
    expect_connection: bool,
    peerd: Option<ServiceId>,
    listening: bool,
}

pub struct SwapdRunning {
    // Peerd is some, if the running swap has a connection through this peerd.
    // It can change values for listener-spawned connections.
    // Peerd is None, in case of a restored swap. It can change to some either
    // through the Connect command or by registering an incoming connection with
    // an expected counterparty node id.
    peerd: Option<ServiceId>,
    deal: Deal,
    arbitrating_syncer: ServiceId,
    accordant_syncer: ServiceId,
    swap_id: SwapId,
    // This is Some if funding is required for this swap, None if not.
    funding_info: Option<FundingInfo>,
    // Tracks the auto-funding status of the swap.
    auto_funded: bool,
    // A list of clients to report back to on Connect success.
    clients_awaiting_connect_result: Vec<ServiceId>,
    trade_role: TradeRole,
    // Some for a restore Maker swap, None otherwise.
    expected_counterparty_node_id: Option<NodeId>,
}

#[derive(Clone)]
pub enum ConsumedDealRole {
    Taker,
    Maker(Commit),
}

impl From<ConsumedDealRole> for TradeRole {
    fn from(consumed_deal: ConsumedDealRole) -> Self {
        match consumed_deal {
            ConsumedDealRole::Taker => TradeRole::Taker,
            ConsumedDealRole::Maker(_) => TradeRole::Maker,
        }
    }
}

impl StateMachine<Runtime, Error> for TradeStateMachine {
    fn next(self, event: Event, runtime: &mut Runtime) -> Result<Option<Self>, Error> {
        let log_helper = self.log_helper();
        log_helper.log_debug(format!(
            "Checking event request {} from {} for state transition",
            event.request, event.source
        ));
        match self {
            TradeStateMachine::StartTaker => {
                attempt_transition_to_taker_connect_or_take_deal(event, runtime, log_helper)
            }
            TradeStateMachine::StartMaker => {
                attempt_transition_to_make_deal(event, runtime, log_helper)
            }
            TradeStateMachine::StartRestore => {
                attempt_transition_to_restoring_swapd(event, runtime, log_helper)
            }
            TradeStateMachine::TakerConnect(taker_connect) => {
                attempt_transition_to_take_deal(event, runtime, taker_connect, log_helper)
            }
            TradeStateMachine::MakeDeal(make_deal) => {
                attempt_transition_to_taker_committed(event, runtime, make_deal, log_helper)
            }
            TradeStateMachine::TakerCommit(taker_commit) => {
                attempt_transition_from_taker_commit_to_swapd_launched(
                    event,
                    runtime,
                    taker_commit,
                    log_helper,
                )
            }
            TradeStateMachine::TakeDeal(take_deal) => {
                attempt_transition_from_take_deal_to_swapd_launched(
                    event, runtime, take_deal, log_helper,
                )
            }
            TradeStateMachine::SwapdLaunched(swapd_launched) => {
                attempt_transition_from_swapd_launched_to_swapd_running(
                    event,
                    runtime,
                    swapd_launched,
                    log_helper,
                )
            }
            TradeStateMachine::RestoringSwapd(restoring_swapd) => {
                attempt_transition_from_restoring_swapd_to_swapd_running(
                    event,
                    runtime,
                    restoring_swapd,
                    log_helper,
                )
            }
            TradeStateMachine::SwapdRunning(swapd_running) => {
                attempt_transition_to_end(event, runtime, swapd_running, log_helper)
            }
        }
    }

    fn name(&self) -> String {
        format!("{} | Trade", self.log_helper().log_prefix())
    }
}

struct LogHelper {
    swap_id: Option<Uuid>,
    swap_role: Option<SwapRole>,
    trade_role: Option<TradeRole>,
}

impl SwapLogging for LogHelper {
    fn swap_details(&self) -> SwapDetails {
        (self.swap_id, self.swap_role, self.trade_role)
    }
}

pub struct TradeStateMachineExecutor {}
impl StateMachineExecutor<Runtime, Error, TradeStateMachine> for TradeStateMachineExecutor {}

impl TradeStateMachine {
    fn log_helper(&self) -> LogHelper {
        LogHelper {
            swap_id: self.uuid(),
            swap_role: self.swap_role(),
            trade_role: self.trade_role(),
        }
    }

    pub fn open_deal(&self) -> Option<Deal> {
        if let TradeStateMachine::MakeDeal(MakeDeal { deal, .. }) = self {
            Some(deal.clone())
        } else {
            None
        }
    }

    pub fn deal(&self) -> Option<&Deal> {
        match self {
            TradeStateMachine::MakeDeal(MakeDeal { deal, .. }) => Some(deal),
            TradeStateMachine::TakerConnect(TakerConnect { deal, .. }) => Some(deal),
            TradeStateMachine::TakeDeal(TakeDeal { deal, .. }) => Some(deal),
            TradeStateMachine::TakerCommit(TakerCommit { deal, .. }) => Some(deal),
            TradeStateMachine::SwapdLaunched(SwapdLaunched { deal, .. }) => Some(deal),
            TradeStateMachine::RestoringSwapd(RestoringSwapd { deal, .. }) => Some(deal),
            TradeStateMachine::SwapdRunning(SwapdRunning { deal, .. }) => Some(deal),
            _ => None,
        }
    }

    pub fn uuid(&self) -> Option<Uuid> {
        self.deal().map(|d| d.id().0)
    }

    pub fn trade_role(&self) -> Option<TradeRole> {
        match self {
            TradeStateMachine::StartTaker => Some(TradeRole::Taker),
            TradeStateMachine::StartMaker => Some(TradeRole::Maker),
            TradeStateMachine::MakeDeal(_) => Some(TradeRole::Maker),
            TradeStateMachine::TakerCommit(_) => Some(TradeRole::Maker),
            TradeStateMachine::TakerConnect(_) => Some(TradeRole::Taker),
            TradeStateMachine::TakeDeal(_) => Some(TradeRole::Taker),
            TradeStateMachine::RestoringSwapd(RestoringSwapd { trade_role, .. }) => {
                Some(*trade_role)
            }
            TradeStateMachine::SwapdLaunched(SwapdLaunched {
                consumed_deal_role, ..
            }) => Some(consumed_deal_role.clone().into()),
            TradeStateMachine::SwapdRunning(SwapdRunning { trade_role, .. }) => Some(*trade_role),
            _ => None,
        }
    }

    pub fn swap_role(&self) -> Option<SwapRole> {
        self.deal()
            .map(|d| self.trade_role().map(|t| d.swap_role(&t)))
            .flatten()
    }

    pub fn consumed_deal(&self) -> Option<(Deal, TradeRole)> {
        match self {
            TradeStateMachine::TakeDeal(TakeDeal { deal, .. }) => {
                Some((deal.clone(), TradeRole::Taker))
            }
            TradeStateMachine::TakerCommit(TakerCommit { deal, .. }) => {
                Some((deal.clone(), TradeRole::Taker))
            }
            TradeStateMachine::TakerConnect(TakerConnect { deal, .. }) => {
                Some((deal.clone(), TradeRole::Taker))
            }
            TradeStateMachine::SwapdLaunched(SwapdLaunched {
                deal,
                consumed_deal_role,
                ..
            }) => Some((deal.clone(), consumed_deal_role.clone().into())),
            TradeStateMachine::RestoringSwapd(RestoringSwapd {
                deal, trade_role, ..
            }) => Some((deal.clone(), *trade_role)),
            TradeStateMachine::SwapdRunning(SwapdRunning {
                deal, trade_role, ..
            }) => Some((deal.clone(), *trade_role)),
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
            TradeStateMachine::TakeDeal(TakeDeal { peerd, .. }) => Some(peerd.clone()),
            TradeStateMachine::TakerCommit(TakerCommit { peerd, .. }) => Some(peerd.clone()),
            TradeStateMachine::SwapdLaunched(SwapdLaunched { peerd, .. }) => Some(peerd.clone()),
            TradeStateMachine::SwapdRunning(SwapdRunning { peerd, .. }) => peerd.clone(),
            _ => None,
        }
    }

    pub fn get_swap_id_with_matching_connection(&self, source: &ServiceId) -> Option<SwapId> {
        if let Some(peer) = self.get_connection() {
            if peer == *source {
                self.swap_id()
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn awaiting_connect_from(&self) -> Option<NodeAddr> {
        match self {
            TradeStateMachine::TakerConnect(taker_connect) => {
                Some(node_addr_from_deal(&taker_connect.deal))
            }
            TradeStateMachine::RestoringSwapd(RestoringSwapd {
                deal,
                trade_role,
                expect_connection,
                ..
            }) => {
                if *trade_role == TradeRole::Taker && *expect_connection {
                    Some(node_addr_from_deal(deal))
                } else {
                    None
                }
            }
            TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                deal,
                trade_role,
                ..
            }) => {
                // Peerd is none, implies a connect event is possible. A connect
                // is only possible to a listening peer as a taker, its address
                // thus has to match the address of the deal.
                if *trade_role == TradeRole::Taker && peerd.is_none() {
                    Some(node_addr_from_deal(deal))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn needs_funding(&self, blockchain: Blockchain) -> Option<FundingInfo> {
        match blockchain {
            Blockchain::Monero => self.needs_funding_monero().map(FundingInfo::Monero),
            Blockchain::Bitcoin => self.needs_funding_bitcoin().map(FundingInfo::Bitcoin),
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

fn attempt_transition_to_make_deal(
    mut event: Event,
    runtime: &mut Runtime,
    log_helper: LogHelper,
) -> Result<Option<TradeStateMachine>, Error> {
    match event.request.clone() {
        BusMsg::Ctl(CtlMsg::MakeDeal(ProtoDeal {
            deal_parameters,
            arbitrating_addr,
            accordant_addr,
            public_addr,
            ..
        })) => {
            // start a listener on the bind_addr
            let bind_addr = match runtime.config.get_bind_addr() {
                Err(err) => {
                    event.complete_ctl(CtlMsg::Failure(Failure {
                        code: FailureCode::Unknown,
                        info: err.to_string(),
                    }))?;
                    return Ok(None);
                }
                Ok(bind_addr) => bind_addr,
            };
            match runtime.listen(bind_addr) {
                Err(err) => {
                    log_helper.log_warn(format!(
                        "Failed to start peerd listen, cannot make deal: {}",
                        err
                    ));
                    event.complete_client_ctl(CtlMsg::Failure(Failure {
                        code: FailureCode::Unknown,
                        info: err.to_string(),
                    }))?;
                    Ok(None)
                }
                Ok(node_id) => {
                    let deal = deal_parameters.to_v1(node_id.public_key(), public_addr);
                    let msg = s!("Deal registered, please share with taker.");
                    log_helper.log_info(format!(
                        "{}: {:#}",
                        "Deal registered.".bright_green_bold(),
                        deal.id().bright_yellow_bold()
                    ));
                    event.send_ctl_service(
                        ServiceId::Database,
                        CtlMsg::SetDealInfo(DealInfo {
                            deal: deal.clone(),
                            serialized_deal: deal.to_string(),
                            status: DealStatus::Open,
                            local_trade_role: TradeRole::Maker,
                        }),
                    )?;
                    event.complete_client_info(InfoMsg::MadeDeal(MadeDeal {
                        message: msg,
                        viewable_deal: ViewableDeal {
                            deal: deal.to_string(),
                            details: deal.clone(),
                        },
                    }))?;
                    runtime.deals.insert(deal.clone());
                    Ok(Some(TradeStateMachine::MakeDeal(MakeDeal {
                        deal,
                        arb_addr: arbitrating_addr,
                        acc_addr: accordant_addr,
                    })))
                }
            }
        }
        req => {
            log_helper.log_warn(format!(
                "Request {} from {} invalid for state start maker - invalidating.",
                req, event.source
            ));
            Ok(None)
        }
    }
}

fn attempt_transition_to_taker_connect_or_take_deal(
    mut event: Event,
    runtime: &mut Runtime,
    log_helper: LogHelper,
) -> Result<Option<TradeStateMachine>, Error> {
    match event.request.clone() {
        BusMsg::Ctl(CtlMsg::TakeDeal(PubDeal {
            deal,
            bitcoin_address: arb_addr,
            monero_address: acc_addr,
        })) => {
            if runtime.consumed_deals_contains(&deal) || runtime.deals.contains(&deal) {
                let msg = format!(
                    "{} already exists or was already taken, ignoring request",
                    &deal.to_string()
                );
                log_helper.log_warn(format!("{}", msg.err()));
                event.complete_client_ctl(CtlMsg::Failure(Failure {
                    code: FailureCode::Unknown,
                    info: msg,
                }))?;
                return Ok(None);
            }

            let peer_node_addr = node_addr_from_deal(&deal);
            // connect to the remote peer
            match runtime.connect_peer(&peer_node_addr) {
                Err(err) => {
                    log_helper.log_warn(format!(
                        "Error connecting to remote peer {}, failed to take deal.",
                        err
                    ));
                    event.complete_client_ctl(CtlMsg::Failure(Failure {
                        code: FailureCode::Unknown,
                        info: err.to_string(),
                    }))?;
                    Ok(None)
                }
                Ok((connected, peer_service_id)) => {
                    if connected {
                        let deal_registered = "Deal registered".to_string();
                        log_helper.log_info(format!(
                            "{}: {:#}",
                            deal_registered.bright_green_bold(),
                            &deal.id().bright_yellow_bold()
                        ));
                        event.send_ctl_service(
                            ServiceId::Wallet,
                            CtlMsg::CreateSwapKeys(deal.clone(), runtime.wallet_token.clone()),
                        )?;
                        event.complete_client_info(InfoMsg::TookDeal(TookDeal {
                            deal_id: deal.id(),
                            message: deal_registered,
                        }))?;
                        runtime.deals.insert(deal.clone());
                        Ok(Some(TradeStateMachine::TakeDeal(TakeDeal {
                            deal,
                            arb_addr,
                            acc_addr,
                            peerd: peer_service_id,
                        })))
                    } else {
                        Ok(Some(TradeStateMachine::TakerConnect(TakerConnect {
                            deal,
                            arb_addr,
                            acc_addr,
                            source: event.source,
                        })))
                    }
                }
            }
        }
        req => {
            log_helper.log_warn(format!(
                "Request {} from {} invalid for state start restore - invalidating.",
                req, event.source,
            ));
            Ok(None)
        }
    }
}

fn attempt_transition_to_restoring_swapd(
    mut event: Event,
    runtime: &mut Runtime,
    log_helper: LogHelper,
) -> Result<Option<TradeStateMachine>, Error> {
    // check if databased and walletd are running
    match event.request.clone() {
        BusMsg::Ctl(CtlMsg::RestoreCheckpoint(CheckpointEntry {
            swap_id,
            deal,
            trade_role,
            expected_counterparty_node_id,
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
                let peer_node_addr = node_addr_from_deal(&deal);
                if let Err(err) = runtime.connect_peer(&peer_node_addr) {
                    log_helper.log_warn(format!("failed to reconnect to peer on restore: {}", err));
                    false
                } else {
                    true
                }
            } else {
                false
            };

            let swap_config = runtime.config.get_swap_config(
                deal.parameters.arbitrating_blockchain.try_into()?,
                deal.parameters.accordant_blockchain.try_into()?,
                deal.parameters.network,
            )?;

            let listening = if trade_role == TradeRole::Maker {
                match runtime.config.get_bind_addr() {
                    Ok(bind_addr) => {
                        if let Err(err) = runtime.listen(bind_addr) {
                            log_helper.log_warn(format!("failed to re-listen on restore: {}", err));
                            false
                        } else {
                            true
                        }
                    }
                    Err(err) => {
                        log_helper.log_error(format!(
                            "Failed to relisten on restore, bad bind address configuration: {}",
                            err
                        ));
                        false
                    }
                }
            } else {
                false
            };

            let arbitrating_syncer_up = syncer_up(
                &mut runtime.spawning_services,
                &mut runtime.registered_services,
                deal.parameters.arbitrating_blockchain,
                deal.parameters.network,
                &runtime.config,
            )?;
            let accordant_syncer_up = syncer_up(
                &mut runtime.spawning_services,
                &mut runtime.registered_services,
                deal.parameters.accordant_blockchain,
                deal.parameters.network,
                &runtime.config,
            )?;

            launch_swapd(trade_role, deal.clone(), swap_id, swap_config)?;
            event.complete_client_info(InfoMsg::String("Restoring checkpoint.".to_string()))?;

            Ok(Some(TradeStateMachine::RestoringSwapd(RestoringSwapd {
                swap_id,
                deal,
                trade_role,
                expected_counterparty_node_id,
                arbitrating_syncer_up,
                accordant_syncer_up,
                swapd_up: false,
                peerd: None,
                expect_connection,
                listening,
            })))
        }
        req => {
            log_helper.log_warn(format!(
                "Request {} from {} invalid for state start restore - invalidating.",
                req, event.source,
            ));
            Ok(None)
        }
    }
}

fn attempt_transition_to_taker_committed(
    mut event: Event,
    runtime: &mut Runtime,
    make_deal: MakeDeal,
    log_helper: LogHelper,
) -> Result<Option<TradeStateMachine>, Error> {
    let MakeDeal {
        deal,
        arb_addr,
        acc_addr,
    } = make_deal;
    match (event.request.clone(), event.source.clone()) {
        (BusMsg::P2p(PeerMsg::TakerCommit(taker_commit)), ServiceId::Peer(..)) => {
            if deal == taker_commit.deal {
                let source = event.source.clone();
                log_helper.log_info(
                    "Received TakerCommit for swap - requesting walletd to create swap keys.",
                );
                event.send_ctl_service(
                    ServiceId::Wallet,
                    CtlMsg::CreateSwapKeys(deal.clone(), runtime.wallet_token.clone()),
                )?;
                event.complete_ctl_service(
                    ServiceId::Database,
                    CtlMsg::SetDealInfo(DealInfo {
                        deal: deal.clone(),
                        serialized_deal: deal.to_string(),
                        status: DealStatus::InProgress,
                        local_trade_role: TradeRole::Maker,
                    }),
                )?;
                Ok(Some(TradeStateMachine::TakerCommit(TakerCommit {
                    peerd: source,
                    deal,
                    commit: taker_commit.commit,
                    target_bitcoin_address: arb_addr,
                    target_monero_address: acc_addr,
                })))
            } else {
                log_helper.log_error(format!(
                    "Received invalid TakerCommit for deal {}.",
                    deal.id()
                ));
                Ok(Some(TradeStateMachine::MakeDeal(MakeDeal {
                    deal,
                    arb_addr,
                    acc_addr,
                })))
            }
        }
        (BusMsg::Ctl(CtlMsg::RevokeDeal(revoke_deal)), _) => {
            log_helper.log_debug(format!("attempting to revoke {}", deal));
            if revoke_deal == deal {
                log_helper.log_info(format!("Revoked deal {}", deal.label()));
                event.send_ctl_service(
                    ServiceId::Database,
                    CtlMsg::SetDealInfo(DealInfo {
                        deal: deal.clone(),
                        serialized_deal: deal.to_string(),
                        status: DealStatus::Revoked,
                        local_trade_role: TradeRole::Maker,
                    }),
                )?;
                event.complete_client_info(InfoMsg::String(
                    "Successfully revoked deal.".to_string(),
                ))?;
                Ok(None)
            } else {
                let msg = "Cannot revoke deal, it does not exist".to_string();
                log_helper.log_error(&msg);
                event.complete_client_info(InfoMsg::String(msg))?;
                Ok(Some(TradeStateMachine::MakeDeal(MakeDeal {
                    deal,
                    arb_addr,
                    acc_addr,
                })))
            }
        }
        (req, source) => {
            if let BusMsg::Ctl(CtlMsg::Hello) = req {
                log_helper.log_trace(format!(
                    "BusMsg {} from {} invalid for state make deal.",
                    req, source
                ));
            } else {
                log_helper.log_warn(format!(
                    "BusMsg {} from {} invalid for state make deal.",
                    req, source
                ));
            }
            Ok(Some(TradeStateMachine::MakeDeal(MakeDeal {
                deal,
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
    log_helper: LogHelper,
) -> Result<Option<TradeStateMachine>, Error> {
    let TakerCommit {
        peerd,
        deal,
        commit,
        target_bitcoin_address,
        target_monero_address,
    } = taker_commit;
    match event.request {
        BusMsg::Ctl(CtlMsg::SwapKeys(swap_keys)) => {
            let swap_id = commit.swap_id();
            log_helper.log_info("Creating new swap.");
            let tsm = transition_to_swapd_launched_tsm(
                runtime,
                ConsumedDealRole::Maker(commit),
                swap_keys,
                peerd,
                deal,
                target_bitcoin_address,
                target_monero_address,
                swap_id,
                log_helper,
            )?;
            Ok(Some(tsm))
        }
        req => {
            if let BusMsg::Ctl(CtlMsg::Hello) = req {
                log_helper.log_trace(format!("BusMsg {} from {} invalid for state Taker Commit - expected LaunchSwap request.", req, event.source));
            } else {
                log_helper.log_warn(format!("BusMsg {} from {} invalid for state Taker Commit - expected LaunchSwap request.", req, event.source));
            }
            Ok(Some(TradeStateMachine::TakerCommit(TakerCommit {
                peerd,
                deal,
                commit,
                target_bitcoin_address,
                target_monero_address,
            })))
        }
    }
}

fn attempt_transition_to_take_deal(
    mut event: Event,
    runtime: &mut Runtime,
    taker_connect: TakerConnect,
    log_helper: LogHelper,
) -> Result<Option<TradeStateMachine>, Error> {
    let TakerConnect {
        deal,
        arb_addr,
        acc_addr,
        source,
    } = taker_connect;
    match event.request {
        BusMsg::Ctl(CtlMsg::ConnectSuccess)
            if Some(node_addr_from_deal(&deal)) == event.source.node_addr() =>
        {
            runtime.handle_new_connection(event.source.clone());
            let deal_registered = "Deal registered".to_string();
            log_helper.log_info(format!(
                "{}: {:#}",
                deal_registered.bright_green_bold(),
                &deal.id().bright_yellow_bold()
            ));
            event.send_ctl_service(
                ServiceId::Wallet,
                CtlMsg::CreateSwapKeys(deal.clone(), runtime.wallet_token.clone()),
            )?;
            event.send_client_info(
                source,
                InfoMsg::TookDeal(TookDeal {
                    deal_id: deal.id(),
                    message: deal_registered,
                }),
            )?;
            runtime.deals.insert(deal.clone());
            Ok(Some(TradeStateMachine::TakeDeal(TakeDeal {
                deal,
                arb_addr,
                acc_addr,
                peerd: event.source,
            })))
        }
        BusMsg::Ctl(CtlMsg::ConnectFailed)
            if Some(node_addr_from_deal(&deal)) == event.source.node_addr() =>
        {
            log_helper.log_warn(format!(
                "{} | Connection to the remote peer {} failed, cannot  take the deal.",
                deal.id(),
                event.source
            ));
            runtime.handle_failed_connection(event.endpoints, event.source.clone())?;
            event.send_client_ctl(
                source,
                CtlMsg::Failure(Failure {
                    info: format!("Could not connect to remote peer {}.", event.source),
                    code: FailureCode::Unknown,
                }),
            )?;
            Ok(None)
        }
        req => {
            if let BusMsg::Ctl(CtlMsg::Hello) = req {
                log_helper.log_trace(format!(
                    "BusMsg {} from {} invalid for state Taker Connect - expected ConnectedFailed or ConnectSuccess request.",
                    req,
                    event.source
                ));
            } else {
                log_helper.log_warn(format!(
                    "BusMsg {} from {} invalid for state Taker Connect - expected ConnectFailed or ConnectSuccess request.",
                    req, event.source
                ));
            }
            Ok(Some(TradeStateMachine::TakerConnect(TakerConnect {
                deal,
                arb_addr,
                acc_addr,
                source,
            })))
        }
    }
}

fn attempt_transition_from_take_deal_to_swapd_launched(
    mut event: Event,
    runtime: &mut Runtime,
    take_deal: TakeDeal,
    log_helper: LogHelper,
) -> Result<Option<TradeStateMachine>, Error> {
    let TakeDeal {
        deal,
        arb_addr,
        acc_addr,
        peerd,
    } = take_deal;
    match &event.request {
        BusMsg::Ctl(CtlMsg::SwapKeys(swap_keys)) => {
            let swap_id: SwapId = deal.id().into(); // The deal id is now used to track a swap
            log_helper.log_info("Creating new swap.");
            let tsm = transition_to_swapd_launched_tsm(
                runtime,
                ConsumedDealRole::Taker,
                swap_keys.clone(),
                peerd,
                deal.clone(),
                arb_addr,
                acc_addr,
                swap_id,
                log_helper,
            )?;
            event.send_ctl_service(
                ServiceId::Database,
                CtlMsg::SetDealInfo(DealInfo {
                    serialized_deal: deal.to_string(),
                    deal,
                    status: DealStatus::InProgress,
                    local_trade_role: TradeRole::Taker,
                }),
            )?;
            Ok(Some(tsm))
        }
        req => {
            if let BusMsg::Ctl(CtlMsg::Hello) = req {
                log_helper.log_trace(format!(
                    "BusMsg {} from {} invalid for state Take Deal - expected LaunchSwap request.",
                    req, event.source
                ));
            } else {
                log_helper.log_trace(format!(
                    "BusMsg {} from {} invalid for state Take Deal - expected LaunchSwap request.",
                    req, event.source
                ));
            }
            Ok(Some(TradeStateMachine::TakeDeal(TakeDeal {
                deal,
                arb_addr,
                acc_addr,
                peerd,
            })))
        }
    }
}

fn transition_to_swapd_launched_tsm(
    runtime: &mut Runtime,
    consumed_deal_role: ConsumedDealRole,
    swap_keys: SwapKeys,
    peerd: ServiceId,
    deal: Deal,
    target_bitcoin_address: bitcoin::Address,
    target_monero_address: monero::Address,
    swap_id: SwapId,
    log_helper: LogHelper,
) -> Result<TradeStateMachine, Error> {
    let swap_config = runtime.config.get_swap_config(
        deal.parameters.arbitrating_blockchain.try_into()?,
        deal.parameters.accordant_blockchain.try_into()?,
        deal.parameters.network,
    )?;
    let SwapKeys { key_manager, .. } = swap_keys;
    let arbitrating_syncer_up = syncer_up(
        &mut runtime.spawning_services,
        &mut runtime.registered_services,
        deal.parameters.arbitrating_blockchain,
        deal.parameters.network,
        &runtime.config,
    )?;
    let accordant_syncer_up = syncer_up(
        &mut runtime.spawning_services,
        &mut runtime.registered_services,
        deal.parameters.accordant_blockchain,
        deal.parameters.network,
        &runtime.config,
    )?;
    log_helper.log_trace(format!(
        "launching swapd with swap_id: {}",
        swap_id.bright_yellow_bold()
    ));

    runtime.stats.incr_initiated();
    launch_swapd(
        consumed_deal_role.clone().into(),
        deal.clone(),
        swap_id,
        swap_config,
    )?;

    Ok(TradeStateMachine::SwapdLaunched(SwapdLaunched {
        peerd,
        swap_id,
        deal,
        arbitrating_syncer_up,
        accordant_syncer_up,
        swapd_up: false,
        key_manager,
        target_bitcoin_address,
        target_monero_address,
        consumed_deal_role,
        peerd_reconnected: false,
    }))
}

fn attempt_transition_from_swapd_launched_to_swapd_running(
    mut event: Event,
    runtime: &mut Runtime,
    swapd_launched: SwapdLaunched,
    log_helper: LogHelper,
) -> Result<Option<TradeStateMachine>, Error> {
    let SwapdLaunched {
        mut peerd,
        deal,
        swap_id,
        mut arbitrating_syncer_up,
        mut accordant_syncer_up,
        mut swapd_up,
        consumed_deal_role,
        mut peerd_reconnected,
        target_bitcoin_address,
        target_monero_address,
        key_manager,
    } = swapd_launched;
    match (event.request.clone(), event.source.clone()) {
        (BusMsg::Ctl(CtlMsg::Hello), source)
            if ServiceId::Syncer(Blockchain::Monero, deal.parameters.network) == source =>
        {
            accordant_syncer_up = Some(source);
        }
        (BusMsg::Ctl(CtlMsg::Hello), source) if ServiceId::Swap(swap_id) == source => {
            swapd_up = true;
        }
        (BusMsg::Ctl(CtlMsg::Hello), source)
            if ServiceId::Syncer(Blockchain::Bitcoin, deal.parameters.network) == source =>
        {
            arbitrating_syncer_up = Some(source);
        }
        (BusMsg::Ctl(CtlMsg::Hello), ServiceId::Peer(..)) => {}
        _ => {
            log_helper.log_trace(format!(
                "{} | BusMsg {} invalid for state swapd launched",
                swap_id, event.request
            ));
        }
    }

    let peerd_up = runtime.registered_services.iter().any(|service| {
        if *service == peerd {
            true
        } else if service.node_addr() == peerd.node_addr() {
            // Peerd might have changed after reconnect in the meantime
            peerd = service.clone();
            peerd_reconnected = true;
            true
        } else {
            false
        }
    });

    if let (Some(accordant_syncer), Some(arbitrating_syncer), true, true) = (
        accordant_syncer_up.clone(),
        arbitrating_syncer_up.clone(),
        swapd_up,
        peerd_up,
    ) {
        // Tell swapd swap options and link it with the
        // connection daemon
        log_helper.log_debug(format!(
            "Swap daemon is known: we spawned it to create a swap. BusMsging swapd to be the {} of this swap",
            TradeRole::from(consumed_deal_role.clone()),
        ));
        let init_swap_req = match &consumed_deal_role {
            ConsumedDealRole::Maker(commit) => CtlMsg::MakeSwap(InitMakerSwap {
                peerd: peerd.clone(),
                report_to: runtime.identity(),
                swap_id,
                key_manager,
                target_bitcoin_address,
                target_monero_address,
                commit: commit.clone(),
            }),
            ConsumedDealRole::Taker => CtlMsg::TakeSwap(InitTakerSwap {
                peerd: peerd.clone(),
                report_to: runtime.identity(),
                swap_id,
                key_manager,
                target_bitcoin_address,
                target_monero_address,
            }),
        };
        if peerd_reconnected {
            event.send_client_ctl(
                ServiceId::Swap(swap_id),
                CtlMsg::PeerdReconnected(peerd.clone()),
            )?;
        }
        event.complete_ctl_service(ServiceId::Swap(swap_id), init_swap_req)?;

        Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
            peerd: Some(peerd),
            swap_id,
            accordant_syncer,
            arbitrating_syncer,
            deal,
            funding_info: None,
            auto_funded: false,
            clients_awaiting_connect_result: vec![],
            trade_role: consumed_deal_role.into(),
            expected_counterparty_node_id: None,
        })))
    } else {
        log_helper.log_debug(format!(
            "Not transitioning to SwapdRunning yet, service availability: accordant_syncer: {}, arbitrating_syncer: {}, swapd: {}, peerd: {}",
            accordant_syncer_up.is_some(),
            arbitrating_syncer_up.is_some(),
            swapd_up,
            peerd_up,
        ));

        Ok(Some(TradeStateMachine::SwapdLaunched(SwapdLaunched {
            swap_id,
            deal,
            peerd,
            key_manager,
            target_bitcoin_address,
            target_monero_address,
            arbitrating_syncer_up,
            accordant_syncer_up,
            swapd_up,
            consumed_deal_role,
            peerd_reconnected,
        })))
    }
}

fn attempt_transition_from_restoring_swapd_to_swapd_running(
    mut event: Event,
    runtime: &mut Runtime,
    restoring_swapd: RestoringSwapd,
    log_helper: LogHelper,
) -> Result<Option<TradeStateMachine>, Error> {
    let RestoringSwapd {
        swap_id,
        deal,
        trade_role,
        expected_counterparty_node_id,
        mut arbitrating_syncer_up,
        mut accordant_syncer_up,
        mut swapd_up,
        mut peerd,
        mut expect_connection,
        listening,
    } = restoring_swapd;
    match (event.request.clone(), event.source.clone()) {
        (BusMsg::Ctl(CtlMsg::Hello), source)
            if ServiceId::Syncer(Blockchain::Monero, deal.parameters.network) == source =>
        {
            accordant_syncer_up = Some(source);
        }
        (BusMsg::Ctl(CtlMsg::Hello), source) if ServiceId::Swap(swap_id) == source => {
            swapd_up = true;
        }
        (BusMsg::Ctl(CtlMsg::Hello), source)
            if ServiceId::Syncer(Blockchain::Bitcoin, deal.parameters.network) == source =>
        {
            arbitrating_syncer_up = Some(source);
        }
        (BusMsg::Ctl(CtlMsg::ConnectSuccess), source)
            if Some(node_addr_from_deal(&deal)) == source.node_addr()
                && trade_role == TradeRole::Taker =>
        {
            runtime.handle_new_connection(event.source.clone());

            log_helper.log_info("Peerd connected for restored swap");
            peerd = Some(event.source.clone());
        }
        (BusMsg::Ctl(CtlMsg::ConnectFailed), source)
            if Some(node_addr_from_deal(&deal)) == source.node_addr()
                && trade_role == TradeRole::Taker =>
        {
            runtime.handle_failed_connection(event.endpoints, source)?;
            expect_connection = false;
        }
        (BusMsg::Ctl(CtlMsg::Hello), source) if trade_role == TradeRole::Maker => {
            if let Ok(bind_addr) = runtime.config.get_bind_addr() {
                if let Some(node_id) = expected_counterparty_node_id {
                    if source.node_addr() == Some(NodeAddr::new(node_id, bind_addr)) {
                        log_helper.log_info("Peerd connected for restored swap");
                        peerd = Some(source);
                    }
                }
            } else {
                log_helper.log_error("Invalid bind addr configuration");
            }
        }
        _ => {}
    }
    if let (Some(accordant_syncer), Some(arbitrating_syncer), true, true) = (
        accordant_syncer_up.clone(),
        arbitrating_syncer_up.clone(),
        swapd_up,
        (!expect_connection || peerd.is_some()), // expect_connection implies connected
    ) {
        log_helper.log_info("Restoring swap");
        runtime.stats.incr_initiated();

        if let Some(peerd) = peerd.clone() {
            event.send_ctl_service(ServiceId::Swap(swap_id), CtlMsg::PeerdReconnected(peerd))?;
        }

        event.complete_ctl_service(
            ServiceId::Database,
            CtlMsg::RestoreCheckpoint(CheckpointEntry {
                swap_id,
                deal: deal.clone(),
                trade_role,
                expected_counterparty_node_id,
            }),
        )?;

        Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
            peerd,
            swap_id,
            accordant_syncer,
            arbitrating_syncer,
            deal,
            auto_funded: false,
            funding_info: None,
            clients_awaiting_connect_result: vec![],
            trade_role,
            expected_counterparty_node_id,
        })))
    } else {
        Ok(Some(TradeStateMachine::RestoringSwapd(RestoringSwapd {
            swap_id,
            deal,
            trade_role,
            expected_counterparty_node_id,
            arbitrating_syncer_up,
            accordant_syncer_up,
            swapd_up,
            peerd,
            expect_connection,
            listening,
        })))
    }
}

fn attempt_transition_to_end(
    mut event: Event,
    runtime: &mut Runtime,
    swapd_running: SwapdRunning,
    log_helper: LogHelper,
) -> Result<Option<TradeStateMachine>, Error> {
    let SwapdRunning {
        peerd,
        deal,
        swap_id,
        arbitrating_syncer,
        accordant_syncer,
        funding_info,
        auto_funded,
        mut clients_awaiting_connect_result,
        trade_role,
        expected_counterparty_node_id,
    } = swapd_running;
    match (event.request.clone(), event.source.clone()) {
        (BusMsg::Ctl(CtlMsg::Hello), source)
            if matches!(source, ServiceId::Peer(..))
                && source.node_addr() == peerd.as_ref().and_then(|p| p.node_addr()) =>
        {
            let swap_service_id = ServiceId::Swap(swap_id);
            log_helper.log_debug("Letting swapd know of peer reconnection.");
            event.complete_ctl_service(swap_service_id, CtlMsg::PeerdReconnected(source))?;
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                deal,
                swap_id,
                arbitrating_syncer,
                accordant_syncer,
                funding_info,
                auto_funded,
                clients_awaiting_connect_result,
                trade_role,
                expected_counterparty_node_id,
            })))
        }

        (BusMsg::Ctl(CtlMsg::Hello), source)
            if peerd.is_none()
                && expected_counterparty_node_id == source.node_addr().map(|a| a.id) =>
        {
            let swap_service_id = ServiceId::Swap(swap_id);
            log_helper.log_debug("Letting swapd know of peer reconnection.");
            event.complete_ctl_service(swap_service_id, CtlMsg::PeerdReconnected(source))?;
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                deal,
                swap_id,
                arbitrating_syncer,
                accordant_syncer,
                funding_info,
                auto_funded,
                clients_awaiting_connect_result,
                trade_role,
                expected_counterparty_node_id,
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
                    log_helper.log_info("Attempting to auto-fund Bitcoin");
                    log_helper.log_debug(format!("Auto funding config: {:#?}", auto_fund_config));

                    use bitcoincore_rpc::{Auth, Client, Error, RpcApi};
                    use std::path::PathBuf;

                    let host = auto_fund_config.bitcoin_rpc;
                    let bitcoin_rpc = match auto_fund_config.bitcoin_cookie_path {
                            Some(cookie) => {
                                let path = PathBuf::from_str(&shellexpand::tilde(&cookie)).unwrap();
                                log_helper.log_debug("bitcoin-rpc connecting with cookie auth");
                                Client::new(&host, Auth::CookieFile(path))
                            }
                            None => {
                                match (auto_fund_config.bitcoin_rpc_user, auto_fund_config.bitcoin_rpc_pass) {
                                    (Some(rpc_user), Some(rpc_pass)) => {
                                        log_helper.log_debug("bitcoin-rpc connecting with userpass auth");
                                        Client::new(&host, Auth::UserPass(rpc_user, rpc_pass))
                                    }
                                    _ => {
                                        log_helper.log_error(
                                            "Couldn't instantiate Bitcoin RPC - provide either `bitcoin_cookie_path` or `bitcoin_rpc_user` AND `bitcoin_rpc_pass` configuration parameters",
                                        );
                                        Err(Error::InvalidCookieFile)}
                                }
                            }
                        }.unwrap();

                    match bitcoin_rpc
                        .send_to_address(address, amount, None, None, None, None, None, None)
                    {
                        Ok(txid) => {
                            log_helper.log_info(format!(
                                "Auto-funded Bitcoin with txid: {}",
                                txid.tx_hash()
                            ));
                            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                                peerd,
                                deal,
                                swap_id,
                                arbitrating_syncer,
                                accordant_syncer,
                                funding_info: Some(info),
                                auto_funded: true,
                                clients_awaiting_connect_result,
                                trade_role,
                                expected_counterparty_node_id,
                            })))
                        }
                        Err(err) => {
                            log_helper.log_error(format!(
                                    "Auto-funding Bitcoin transaction failed, pushing to cli, use `swap-cli needs-funding Bitcoin` to retrieve address and amount: {}",
                                    err
                                ));
                            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                                peerd,
                                deal,
                                swap_id,
                                arbitrating_syncer,
                                accordant_syncer,
                                funding_info: Some(info),
                                auto_funded: false,
                                clients_awaiting_connect_result,
                                trade_role,
                                expected_counterparty_node_id,
                            })))
                        }
                    }
                } else {
                    Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                        peerd,
                        deal,
                        swap_id,
                        arbitrating_syncer,
                        accordant_syncer,
                        funding_info: Some(info.clone()),
                        auto_funded: false,
                        clients_awaiting_connect_result,
                        trade_role,
                        expected_counterparty_node_id,
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
                    log_helper.log_info("Attempting to auto-fund Monero");
                    log_helper.log_debug(format!("Auto funding config: {:#?}", auto_fund_config));
                    use tokio::runtime::Builder;
                    let rt = Builder::new_multi_thread()
                        .worker_threads(1)
                        .enable_all()
                        .build()
                        .unwrap();
                    rt.block_on(async {
                        let host = auto_fund_config.monero_rpc_wallet;
                        let wallet = monero_rpc::RpcClientBuilder::new()
                            .build(host)
                            .expect("client builder failed, cannot recover from bad configuration")
                            .wallet();
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
                                    log_helper.log_info(format!(
                                        "Auto-funded Monero with txid: {}",
                                        tx.tx_hash.tx_hash()
                                    ));
                                    auto_funded = true;
                                    break;
                                }
                                Err(err) => {
                                    if (err.to_string().contains("not enough") && err.to_string().contains("money")) || retries == 0 {
                                        log_helper.log_error(format!("Auto-funding Monero transaction failed, pushing to cli, use `swap-cli needs-funding Monero` to retrieve address and amount: {}", err));
                                        break;
                                    } else {
                                        log_helper.log_warn(format!("Auto-funding Monero transaction failed with {}, retrying, {} retries left", err, retries));
                                        tokio::time::sleep(std::time::Duration::from_millis(10 * retries)).await;
                                    }
                                }
                            }
                        }
                        Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                             peerd,
                             deal,
                             swap_id,
                             arbitrating_syncer,
                             accordant_syncer,
                             funding_info: Some(info),
                             auto_funded,
                             clients_awaiting_connect_result,
                             trade_role,
                             expected_counterparty_node_id,
                         })))
                    })
                } else {
                    Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                        peerd,
                        deal,
                        swap_id,
                        arbitrating_syncer,
                        accordant_syncer,
                        funding_info: Some(info),
                        auto_funded: false,
                        clients_awaiting_connect_result,
                        trade_role,
                        expected_counterparty_node_id,
                    })))
                }
            }
        },

        (BusMsg::Ctl(CtlMsg::FundingCompleted(blockchain)), _) => {
            runtime.stats.incr_funded(&blockchain, &swap_id);
            log_helper.log_info(format!("Your {} funding completed", blockchain.label()));
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                deal,
                swap_id,
                arbitrating_syncer,
                accordant_syncer,
                funding_info: None,
                auto_funded,
                clients_awaiting_connect_result,
                trade_role,
                expected_counterparty_node_id,
            })))
        }

        (BusMsg::Ctl(CtlMsg::FundingCanceled(blockchain)), _) => {
            runtime.stats.incr_funding_canceled(&blockchain, &swap_id);
            log_helper.log_info(format!("Your {} funding was canceled.", blockchain.label()));
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                deal,
                swap_id,
                arbitrating_syncer,
                accordant_syncer,
                funding_info: None,
                auto_funded,
                clients_awaiting_connect_result,
                trade_role,
                expected_counterparty_node_id,
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
                let peer_node_addr = node_addr_from_deal(&deal);
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
                deal,
                swap_id,
                arbitrating_syncer,
                accordant_syncer,
                funding_info,
                auto_funded,
                clients_awaiting_connect_result,
                trade_role,
                expected_counterparty_node_id,
            })))
        }

        // A ConnectSuccess event can only come from a peerd connecting to a listener (maker)
        (BusMsg::Ctl(CtlMsg::ConnectSuccess), source)
            if source.node_addr() == Some(node_addr_from_deal(&deal)) =>
        {
            for client in clients_awaiting_connect_result.drain(..) {
                event.send_client_ctl(client, CtlMsg::ConnectSuccess)?;
            }
            runtime.handle_new_connection(source.clone());
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd: Some(source),
                deal,
                swap_id,
                arbitrating_syncer,
                accordant_syncer,
                funding_info,
                auto_funded,
                clients_awaiting_connect_result: vec![],
                trade_role,
                expected_counterparty_node_id,
            })))
        }

        // A ConnectFailed event can only come from a peerd connecting to a listener (maker)
        (BusMsg::Ctl(CtlMsg::ConnectFailed), source)
            if source.node_addr() == Some(node_addr_from_deal(&deal)) =>
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
                deal,
                swap_id,
                arbitrating_syncer,
                accordant_syncer,
                funding_info,
                auto_funded,
                clients_awaiting_connect_result: vec![],
                trade_role,
                expected_counterparty_node_id,
            })))
        }

        (BusMsg::Ctl(CtlMsg::PeerdUnreachable(peerd_service)), source)
            if ServiceId::Swap(swap_id) == source =>
        {
            if runtime.registered_services.contains(&peerd_service) {
                log_helper.log_warn(format!(
                    "Peerd {} was reported to be unreachable, attempting to
                    terminate to kick-off re-connect procedure, if we are
                    taker and the swap is still running.",
                    peerd_service
                ));
                runtime.handle_failed_connection(event.endpoints, peerd_service.clone())?;
                event.complete_ctl_service(peerd_service, CtlMsg::Terminate)?;
            }
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                deal,
                swap_id,
                arbitrating_syncer,
                accordant_syncer,
                funding_info,
                auto_funded,
                clients_awaiting_connect_result,
                trade_role,
                expected_counterparty_node_id,
            })))
        }

        (BusMsg::Ctl(CtlMsg::SwapOutcome(outcome)), source)
            if ServiceId::Swap(swap_id) == source =>
        {
            event.send_ctl_service(
                ServiceId::Database,
                CtlMsg::SetDealInfo(DealInfo {
                    serialized_deal: deal.to_string(),
                    deal,
                    status: DealStatus::Ended(outcome.clone()),
                    local_trade_role: trade_role,
                }),
            )?;
            runtime.clean_up_after_swap(&swap_id, event.endpoints)?;
            runtime.stats.incr_outcome(&outcome);
            match outcome {
                Outcome::SuccessSwap => {
                    log_helper.log_debug(format!("Success on swap {}", swap_id));
                }
                Outcome::FailureRefund => {
                    log_helper.log_warn(format!("Refund on swap {}", swap_id));
                }
                Outcome::FailurePunish => {
                    log_helper.log_warn(format!("Punish on swap {}", swap_id));
                }
                Outcome::FailureAbort => {
                    log_helper.log_warn(format!("Aborted swap {}", swap_id));
                }
            }
            runtime.stats.success_rate();
            Ok(None)
        }

        (req, source) => {
            if let BusMsg::Ctl(CtlMsg::Hello) = req {
                log_helper.log_trace(format!(
                    "BusMsg {} from {} invalid for state swapd running.",
                    req, source
                ));
            } else {
                log_helper.log_warn(format!(
                    "BusMsg {} from {} invalid for state Swapd Running.",
                    req, source
                ));
            }
            Ok(Some(TradeStateMachine::SwapdRunning(SwapdRunning {
                peerd,
                deal,
                swap_id,
                arbitrating_syncer,
                accordant_syncer,
                funding_info,
                auto_funded,
                clients_awaiting_connect_result,
                trade_role,
                expected_counterparty_node_id,
            })))
        }
    }
}

fn node_addr_from_deal(deal: &Deal) -> NodeAddr {
    NodeAddr {
        id: NodeId::from(deal.node_id), // node_id is bitcoin::Pubkey
        addr: deal.peer_address,        // peer_address is InetSocketAddr
    }
}
