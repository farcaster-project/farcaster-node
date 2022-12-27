// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use std::cmp::Ordering;

use bitcoin::{psbt::serialize::Deserialize, secp256k1::ecdsa::Signature};
use farcaster_core::{
    blockchain::Blockchain,
    role::SwapRole,
    swap::btcxmr::{
        message::{
            BuyProcedureSignature, CommitAliceParameters, CommitBobParameters, CoreArbitratingSetup,
        },
        Parameters,
    },
    transaction::TxLabel,
};
use microservices::esb::Handler;
use monero::ViewPair;
use strict_encoding::{StrictDecode, StrictEncode};

use crate::{
    bus::{ctl::MoneroFundingInfo, p2p::Reveal},
    syncerd::AddressTransaction,
};
use crate::{
    bus::{
        ctl::{CtlMsg, InitMakerSwap, InitTakerSwap},
        p2p::{Commit, PeerMsg, TakerCommit},
        BusMsg, Failure, FailureCode,
    },
    event::{Event, StateMachine},
    service::Reporter,
    syncerd::{FeeEstimation, FeeEstimations, SweepAddress, TaskAborted, Txid},
    ServiceId,
};
use crate::{
    bus::{
        ctl::{FundingInfo, Tx},
        info::InfoMsg,
    },
    swapd::{
        runtime::aggregate_xmr_spend_view,
        swap_key_manager::{HandleBuyProcedureSignatureRes, HandleRefundProcedureSignaturesRes},
        syncer_client::{log_tx_created, log_tx_seen},
    },
    syncerd::{Abort, Boolean, SweepSuccess, Task, TaskTarget, TransactionConfirmations},
    Endpoints, Error,
};
use crate::{
    bus::{sync::SyncMsg, Outcome},
    LogStyle,
};
use crate::{
    swapd::swap_key_manager::HandleCoreArbitratingSetupRes, syncerd::types::Event as SyncEvent,
};

use super::{
    runtime::Runtime,
    swap_key_manager::{AliceSwapKeyManager, BobSwapKeyManager, WrappedEncryptedSignature},
};

/// State machine for running a swap.
///
/// State machine automaton: The states are named after the message that
/// triggers their transition. Note that the BobAbortAwaitingBitcoinSweep state
/// is not shown in the diagram, as it would overcomplicate the diagram for little gain.
/// So merely note that Bob can abort at any time before he transitions to
/// BobRefundProcedureSignatures.
///
/// ```ignore
///                           StartMaker                            StartTaker
///                               |                                     |
///                        _______|________                   __________|_________
///                       |                |                 |                    |
///                       |                |                 |                    |
///                       V                V                 V                    V
///                BobInitMaker    AliceInitMaker       BobInitTaker        AliceInitTaker
///                       |                |                 |                    |
///                       |                |                 |                    |
///                       |                |                 V                    V
///                       |                |        BobTakerMakerCommit  AliceTakerMakerCommit
///                       |                |                 |                    |
///                       |_______________ | ________________|                    |
///                               |        |                                      |
///                               |        |______________________________________|
///                               |                                   |
///                               |                                   |
///                               V                                   V
///                           BobReveal                          AliceReveal
///                               |                                   |
///                               |                                   |
///                               V                                   V
///                        BobFeeEstimated                AliceCoreArbitratingSetup
///                               |                                   |
///                               |                                   |
///                               V                                   |
///                           BobFunded                               |
///                               |                                   |_____________________
///                               |                                   |                     |
///                               V                                   V                     |
///                  BobRefundProcedureSignatures          AliceArbitratingLockFinal        |
///        _______________________|                                   |_____________________|
///       |                       |                                   |                     |
///       |                       V                                   V                     |
///       |                BobAccordantLock                    AliceAccordantLock           |
///       |_______________________|                                   |_____________________|
///       |                       |                                   |                     |
///       |                       V                                   V                     |
///       |              BobAccordantLockFinal             AliceBuyProcedureSignature       V
///       |_______________________|                                   |                AliceCancel
///       |                       |                                   |                     |
///       |                       V                                   |         ____________|
///       V                   BobBuySeen                             |        |            |
///   BobCancel                   |                                   |        |            |
///       |                       |                                   |        |            |
///       |                       V                                   |        V            |
///       |                 BobBuySweeping                            |   AliceRefund       |
///       |                       |___________________________________|        |            |
///       |                                          |                         |            |
///       V                                          |                         V            |
///   BobRefund                                      |               AliceRefundSweeping    |
///       |                                          |                         |            |
///       |__________________________________________|_________________________|____________|
///                                                  |
///                                                  |
///                                                  V
///                                               SwapEnd
///         
/// ```

#[derive(Debug, Display, Clone, StrictDecode, StrictEncode)]
pub enum SwapStateMachine {
    /*
        Start States
    */
    // StartTaker state - transitions to AliceInitTaker or BobInitTaker on
    // request TakeSwap, or Swap End on AbortSwap. Triggers watch fee and
    // height.  Creates new taker swap_key_manager. Sends TakerCommit to the counterparty
    // peer.
    #[display("Start {0} Taker")]
    StartTaker(SwapRole),
    // StartMaker state - transition to AliceInitMaker or BobInitMaker on
    // request MakeSwap, or Swap End on AbortSwap. Triggers watch fee and
    // height.  Creates new maker swap_key_manager. Sends MakerCommit to the counterparty
    // peer.
    #[display("Start {0} Maker")]
    StartMaker(SwapRole),

    /*
        Maker States
    */
    // BobInitMaker state - transitions to BobReveal on request Reveal, or
    // Bob Awaiting Bitcoin Sweep on AbortSwap. Sends FundingInfo to
    // farcasterd, watches funding address.
    #[display("Bob Init Maker")]
    BobInitMaker(BobInitMaker),
    // AliceInitMaker state - transitions to AliceReveal on request Reveal, or Swap End
    // on AbortSwap. Sends Reveal to the counterparty peer.
    #[display("Alice Init Maker")]
    AliceInitMaker(AliceInitMaker),

    /*
        Taker States
    */
    // BobInitTaker state - transitions to BobTakerMakerCommit on request
    // MakerCommit, or BobAwaitingBitcoinSweep on AbortSwap.  Watches funding
    // address, sends Reveal to the counterparty peer.
    #[display("Bob Init Taker")]
    BobInitTaker(BobInitTaker),
    // AliceInitTaker state - transitions to AliceTakerMakerCommit on request
    // MakerCommit, or Swap End on AbortSwap.  Sends Reveal to the counterparty
    // peer.
    #[display("Alice Init Taker")]
    AliceInitTaker(AliceInitTaker),
    // BobTakerMakerCommit - transitions to BobReveal on request Reveal, or
    // BobAwaitingBitcoinSweep on request AbortSwap. Sends FundingInfo to
    // farcasterd, watches funding address.
    #[display("Bob Taker Maker Commit")]
    BobTakerMakerCommit(BobTakerMakerCommit),
    // AliceTakerMakerCommit - transitions to AliceReveal on request Reveal, or
    // SwapEnd on request AbortSwap.
    #[display("Alice Taker Maker Commit")]
    AliceTakerMakerCommit(AliceTakerMakerCommit),

    /*
        Bob Happy Path States
    */
    // BobReveal state - transitions to BobFeeEstimated on event FeeEstimation,
    // or Bob AwaitingBitcoinSweep on request AbortSwap or in case of incorrect
    // funding amount. Sends FundingCompleted to Farcasterd, Reveal to
    // counterparty peer, watches Lock, Cancel and Refund, checkpoints the Bob
    // pre Lock state, and sends the CoreArbitratingSetup to the counterparty
    // peer.
    #[display("Bob Reveal")]
    BobReveal(BobReveal),
    // BobFeeEstimated state - transitions to BobFunded on event AddressTransaction
    // or BobAbortAwaitingBitcoinSweep on request AbortSwap or in case of incorrect
    // funding amount.
    #[display("Bob Fee Estimated")]
    BobFeeEstimated(BobFeeEstimated),
    // BobFunded state - transitions to BobRefundProcedureSignatures on request
    // RefundProcedureSignatures or BobAbortAwaitingBitcoinSweep on request AbortSwap.
    // Broadcasts Lock, watches AccLock, watches Buy, checkpoints the Bob pre
    // Buy state.
    #[display("Bob Funded")]
    BobFunded(BobFunded),
    // BobRefundProcedureSignatures state - transitions to BobAccordantLock on event
    // AddressTransaction, or BobCanceled on event TransactionConfirmations.
    // Watches Monero transaction, aborts Monero AddressTransaction task.
    #[display("Bob Refund Procedure Signatures")]
    BobRefundProcedureSignatures(BobRefundProcedureSignatures),
    // BobAccordantLock state - transitions to BobAccordantLockFinal on event
    // TransactionConfirmations, or BobCanceled on event
    // TransactionConfirmations. Sends BuyProcedureSignature to counterparty
    // peer.
    #[display("Bob Accordant Lock")]
    BobAccordantLock(BobAccordantLock),
    // BobAccordantLockFinal state - transitions to BobAccordantFinal on event
    // TransactionConfirmations, BobBuySeen on event TransactionRetrieved, or
    // BobCanceled on event TransactionConfirmations. Retrieves Buy transaction.
    #[display("Bob Accordant Lock Final")]
    BobAccordantLockFinal(BobAccordantLockFinal),
    // BobBuySeen state - transitions to BobBuySweeping on event
    // TransactionConfirmations. Sends sweep Monero to Monero syncer.
    #[display("Bob Buy Seen")]
    BobBuySeen(SweepAddress),
    // BobBuySweeping state - transitions to SwapEnd on request SweepSuccess.
    // Cleans up remaining swap data and report to Farcasterd.
    #[display("Bob Buy Sweeping")]
    BobBuySweeping,

    /*
        Bob Cancel States
    */
    // BobCanceled state - transitions to BobCancelFinal on event
    // TransactionConfirmations. Broadcasts the Refund transaction.
    #[display("Bob Cancel")]
    BobCanceled,
    // BobCancelFinal state - transitions to SwapEnd on event
    // AddressTransaction. Cleans up remaining swap data and report to
    // Farcasterd.
    #[display("Bob Cancel Final")]
    BobCancelFinal,

    /*
        Bob Abort State
    */
    // BobAbortAwaitingBitcoinSweep state - transitions to SwapEnd on event
    // SweepSuccess. Cleans up remaning swap data and report to Farcasterd.
    #[display("Bob Abort Awaiting Bitcoin Sweep")]
    BobAbortAwaitingBitcoinSweep,

    /*
        Alice Happy Path States
    */
    // AliceReveal state - transitions to AliceCoreArbitratingSetup on message
    // CoreArbitratingSetup, or SwapEnd on request AbortSwap. Watches Lock,
    // Cancel, and Refund transactions, checkpoints Alice pre Lock Bob. Sends
    // the RefundProcedureSignature message to the counterparty peer.
    #[display("Alice Reveal")]
    AliceReveal(AliceReveal),
    // AliceCoreArbitratingSetup state - transitions to
    // AliceArbitratingLockFinal on event TransactionConfirmations, or
    // AliceCanceled on event TransactionConfirmations. Watches Monero funding
    // address.
    #[display("Alice Core Arbitrating Setup")]
    AliceCoreArbitratingSetup(AliceCoreArbitratingSetup),
    // AliceArbitratingLockFinal state - transitions to AliceAccordantLock on
    // event AddressTransaction, to AliceCoreArbitratingSetup on event Empty and
    // TransactionConfirmations, or to AliceCanceled on
    // TransactionConfirmations. Completes Funding, watches Monero transaction,
    // aborts watch address.
    #[display("Alice Arbitrating Lock Final")]
    AliceArbitratingLockFinal(AliceArbitratingLockFinal),
    // AliceAccordantLock state - transitions to AliceBuyProcedureSignature on
    // message BuyProcedureSignature, or to AliceCanceled on
    // TransactionConfirmations. Broadcasts Buy transaction, checkpoints Alice
    // pre Buy.
    #[display("Alice Accordant Lock")]
    AliceAccordantLock(AliceAccordantLock),
    // AliceBuyProcedureSignature state - transitions to SwapEnd on event
    // TransactionConfirmations. Cleans up remaining swap data and report to
    // Farcasterd.
    #[display("Alice Buy Procedure Signature")]
    AliceBuyProcedureSignature,

    /*
        Alice Cancel States
    */
    // AliceCanceled state - transitions to AliceRefund or SwapEnd on event
    // TransactionConfirmations. Broadcasts punish transaction or retrieves
    // Refund transaction.
    #[display("Alice Cancel")]
    AliceCanceled(AliceCanceled),
    // AliceRefund state - transitions to AliceRefundSweeping on event
    // TransactionConfirmations. Submits sweep Monero address task.
    #[display("Alice Refund")]
    AliceRefund(SweepAddress),
    // AliceRefundSweeping state - transitions to SwapEnd on event SweepSuccess.
    // Cleans up remaining swap data and reports to Farcasterd.
    #[display("Alice Refund Sweeping")]
    AliceRefundSweeping,

    // End state
    #[display("Swap End: {0}")]
    SwapEnd(Outcome),
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct BobInitMaker {
    remote_commit: CommitAliceParameters,
    swap_key_manager: BobSwapKeyManager,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct AliceInitMaker {
    remote_commit: CommitBobParameters,
    swap_key_manager: AliceSwapKeyManager,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct BobInitTaker {
    swap_key_manager: BobSwapKeyManager,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct AliceInitTaker {
    swap_key_manager: AliceSwapKeyManager,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct AliceTakerMakerCommit {
    remote_commit: CommitBobParameters,
    swap_key_manager: AliceSwapKeyManager,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct BobTakerMakerCommit {
    remote_commit: CommitAliceParameters,
    swap_key_manager: BobSwapKeyManager,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct BobReveal {
    remote_params: Parameters,
    swap_key_manager: BobSwapKeyManager,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct AliceReveal {
    remote_params: Parameters,
    swap_key_manager: AliceSwapKeyManager,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct BobFeeEstimated {
    required_funding_amount: bitcoin::Amount,
    remote_params: Parameters,
    swap_key_manager: BobSwapKeyManager,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct BobFunded {
    remote_params: Parameters,
    core_arbitrating_setup: CoreArbitratingSetup,
    swap_key_manager: BobSwapKeyManager,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct AliceCoreArbitratingSetup {
    remote_params: Parameters,
    core_arbitrating_setup: CoreArbitratingSetup,
    alice_cancel_signature: Signature,
    adaptor_refund: WrappedEncryptedSignature,
    swap_key_manager: AliceSwapKeyManager,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct BobRefundProcedureSignatures {
    remote_params: Parameters,
    swap_key_manager: BobSwapKeyManager,
    buy_procedure_signature: BuyProcedureSignature,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct AliceArbitratingLockFinal {
    swap_key_manager: AliceSwapKeyManager,
    funding_info: MoneroFundingInfo,
    required_funding_amount: monero::Amount,
    remote_params: Parameters,
    core_arbitrating_setup: CoreArbitratingSetup,
    alice_cancel_signature: Signature,
    adaptor_refund: WrappedEncryptedSignature,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct BobAccordantLock {
    remote_params: Parameters,
    swap_key_manager: BobSwapKeyManager,
    buy_procedure_signature: BuyProcedureSignature,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct AliceAccordantLock {
    remote_params: Parameters,
    core_arbitrating_setup: CoreArbitratingSetup,
    alice_cancel_signature: Signature,
    adaptor_refund: WrappedEncryptedSignature,
    swap_key_manager: AliceSwapKeyManager,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct BobAccordantLockFinal {
    remote_params: Parameters,
    buy_procedure_signature: BuyProcedureSignature,
    swap_key_manager: BobSwapKeyManager,
}

#[derive(Clone, Debug, StrictEncode, StrictDecode)]
pub struct AliceCanceled {
    remote_params: Parameters,
    adaptor_refund: WrappedEncryptedSignature,
    swap_key_manager: AliceSwapKeyManager,
}

impl StateMachine<Runtime, Error> for SwapStateMachine {
    fn next(self, event: Event, runtime: &mut Runtime) -> Result<Option<Self>, Error> {
        runtime.log_debug(format!(
            "Checking event request {} from {} for state transition",
            event.request, event.source
        ));
        match self {
            SwapStateMachine::StartTaker(swap_role) => {
                attempt_transition_to_init_taker(event, runtime, swap_role)
            }
            SwapStateMachine::StartMaker(swap_role) => {
                attempt_transition_to_init_maker(event, runtime, swap_role)
            }

            SwapStateMachine::BobInitTaker(bob_init_taker) => {
                try_bob_init_taker_to_bob_taker_maker_commit(event, runtime, bob_init_taker)
            }
            SwapStateMachine::AliceInitTaker(alice_init_taker) => {
                try_alice_init_taker_to_alice_taker_maker_commit(event, runtime, alice_init_taker)
            }

            SwapStateMachine::BobTakerMakerCommit(bob_taker_maker_commit) => {
                try_bob_taker_maker_commit_to_bob_reveal(event, runtime, bob_taker_maker_commit)
            }
            SwapStateMachine::AliceTakerMakerCommit(alice_taker_maker_commit) => {
                try_alice_taker_maker_commit_to_alice_reveal(
                    event,
                    runtime,
                    alice_taker_maker_commit,
                )
            }
            SwapStateMachine::BobInitMaker(bob_init_maker) => {
                try_bob_init_maker_to_bob_reveal(event, runtime, bob_init_maker)
            }
            SwapStateMachine::AliceInitMaker(alice_init_maker) => {
                try_alice_init_maker_to_alice_reveal(event, runtime, alice_init_maker)
            }

            SwapStateMachine::BobReveal(bob_reveal) => {
                try_bob_reveal_to_bob_fee_estimated(event, runtime, bob_reveal)
            }
            SwapStateMachine::BobFeeEstimated(bob_fee_estimated) => {
                try_bob_fee_estimated_to_bob_funded(event, runtime, bob_fee_estimated)
            }
            SwapStateMachine::BobFunded(bob_funded) => {
                try_bob_funded_to_bob_refund_procedure_signature(event, runtime, bob_funded)
            }
            SwapStateMachine::BobRefundProcedureSignatures(bob_refund_procedure_signatures) => {
                try_bob_refund_procedure_signatures_to_bob_accordant_lock(
                    event,
                    runtime,
                    bob_refund_procedure_signatures,
                )
            }
            SwapStateMachine::BobAccordantLock(bob_accordant_lock) => {
                try_bob_accordant_lock_to_bob_accordant_lock_final(
                    event,
                    runtime,
                    bob_accordant_lock,
                )
            }
            SwapStateMachine::BobAccordantLockFinal(bob_accordant_lock_final) => {
                try_bob_accordant_lock_final_to_bob_buy_seen(
                    event,
                    runtime,
                    bob_accordant_lock_final,
                )
            }
            SwapStateMachine::BobBuySeen(task) => {
                try_bob_buy_seen_to_bob_buy_sweeping(event, runtime, task)
            }
            SwapStateMachine::BobBuySweeping => try_bob_buy_sweeping_to_swap_end(event, runtime),

            SwapStateMachine::BobCanceled => try_bob_canceled_to_bob_cancel_final(event, runtime),
            SwapStateMachine::BobCancelFinal => try_bob_cancel_final_to_swap_end(event, runtime),

            SwapStateMachine::BobAbortAwaitingBitcoinSweep => {
                try_awaiting_sweep_to_swap_end(event, runtime)
            }

            SwapStateMachine::AliceReveal(alice_reveal) => {
                try_alice_reveal_to_alice_core_arbitrating_setup(event, runtime, alice_reveal)
            }
            SwapStateMachine::AliceCoreArbitratingSetup(alice_core_arbitrating_setup) => {
                try_alice_core_arbitrating_setup_to_alice_arbitrating_lock_final(
                    event,
                    runtime,
                    alice_core_arbitrating_setup,
                )
            }
            SwapStateMachine::AliceArbitratingLockFinal(alice_arbitrating_lock_final) => {
                try_alice_arbitrating_lock_final_to_alice_accordant_lock(
                    event,
                    runtime,
                    alice_arbitrating_lock_final,
                )
            }
            SwapStateMachine::AliceAccordantLock(alice_accordant_lock) => {
                try_alice_accordant_lock_to_alice_buy_procedure_signature(
                    event,
                    runtime,
                    alice_accordant_lock,
                )
            }
            SwapStateMachine::AliceBuyProcedureSignature => {
                try_alice_buy_procedure_signature_to_swap_end(event, runtime)
            }

            SwapStateMachine::AliceCanceled(alice_canceled) => {
                try_alice_canceled_to_alice_refund_or_alice_punish(event, runtime, alice_canceled)
            }
            SwapStateMachine::AliceRefund(sweep_address) => {
                try_alice_refund_to_alice_refund_sweeping(event, runtime, sweep_address)
            }
            SwapStateMachine::AliceRefundSweeping => {
                try_alice_refund_sweeping_to_swap_end(event, runtime)
            }
            SwapStateMachine::SwapEnd(_) => Ok(None),
        }
    }

    fn name(&self) -> String {
        "Swap State Machine".to_string()
    }
}

pub struct SwapStateMachineExecutor {}
impl SwapStateMachineExecutor {
    pub fn execute(
        runtime: &mut Runtime,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: BusMsg,
        sm: SwapStateMachine,
    ) -> Result<Option<SwapStateMachine>, Error> {
        let request_str = request.to_string();
        let event = Event::with(endpoints, runtime.identity(), source, request);
        let sm_display = sm.to_string();
        let sm_name = sm.name();
        if let Some(new_sm) = sm.next(event, runtime)? {
            let new_sm_display = new_sm.to_string();
            // relegate state transitions staying the same to debug
            if new_sm_display == sm_display {
                runtime.log_info(format!(
                    "{} state self transition {}",
                    sm_name,
                    new_sm.bright_green_bold()
                ));
            } else {
                runtime.log_info(format!(
                    "{} state transition {} -> {}",
                    sm_name,
                    sm_display.red_bold(),
                    new_sm.bright_green_bold()
                ));
            }
            Ok(Some(new_sm))
        } else {
            runtime.log_debug(format!(
                "{} no state change for request {}",
                sm_name, request_str
            ));
            Ok(None)
        }
    }
}

fn attempt_transition_to_init_taker(
    mut event: Event,
    runtime: &mut Runtime,
    swap_role: SwapRole,
) -> Result<Option<SwapStateMachine>, Error> {
    match event.request.clone() {
        BusMsg::Ctl(CtlMsg::TakeSwap(InitTakerSwap {
            ref peerd,
            ref report_to,
            swap_id,
            ref key_manager,
            ref target_bitcoin_address,
            target_monero_address,
        })) => {
            if ServiceId::Swap(swap_id) != runtime.identity {
                runtime.log_error(format!(
                    "This swapd instance is not reponsible for swap_id {}",
                    swap_id
                ));
                return Ok(None);
            };
            // start watching block height changes
            runtime
                .syncer_state
                .watch_height(event.endpoints, Blockchain::Bitcoin)?;
            runtime
                .syncer_state
                .watch_height(event.endpoints, Blockchain::Monero)?;
            runtime.peer_service = peerd.clone();
            if let ServiceId::Peer(0, _) = runtime.peer_service {
                runtime.connected = false;
            } else {
                runtime.connected = true;
            }
            runtime.enquirer = Some(report_to.clone());

            match swap_role {
                SwapRole::Bob => {
                    let mut swap_key_manager = BobSwapKeyManager::new_taker_bob(
                        &mut event,
                        runtime,
                        target_bitcoin_address.clone(),
                        target_monero_address,
                        key_manager.0.clone(),
                    )?;
                    let local_commit =
                        swap_key_manager
                            .taker_commit(&mut event, runtime)
                            .map_err(|err| {
                                runtime.log_error(&err);
                                runtime.report_failure(
                                    event.endpoints,
                                    Failure {
                                        code: FailureCode::Unknown,
                                        info: err.to_string(),
                                    },
                                )
                            })?;
                    let take_swap = TakerCommit {
                        commit: Commit::BobParameters(local_commit),
                        deal: runtime.deal.clone(),
                    };
                    // send taker commit message to counter-party
                    runtime.send_peer(event.endpoints, PeerMsg::TakerCommit(take_swap))?;

                    Ok(Some(SwapStateMachine::BobInitTaker(BobInitTaker {
                        swap_key_manager,
                    })))
                }
                SwapRole::Alice => {
                    let mut swap_key_manager = AliceSwapKeyManager::new_taker_alice(
                        runtime,
                        target_bitcoin_address.clone(),
                        target_monero_address,
                        key_manager.0.clone(),
                    )?;
                    let local_commit =
                        swap_key_manager
                            .taker_commit(&mut event, runtime)
                            .map_err(|err| {
                                runtime.log_error(&err);
                                runtime.report_failure(
                                    event.endpoints,
                                    Failure {
                                        code: FailureCode::Unknown,
                                        info: err.to_string(),
                                    },
                                )
                            })?;
                    let take_swap = TakerCommit {
                        commit: Commit::AliceParameters(local_commit),
                        deal: runtime.deal.clone(),
                    };
                    // send taker commit message to counter-party
                    runtime.send_peer(event.endpoints, PeerMsg::TakerCommit(take_swap))?;

                    Ok(Some(SwapStateMachine::AliceInitTaker(AliceInitTaker {
                        swap_key_manager,
                    })))
                }
            }
        }
        BusMsg::Ctl(CtlMsg::AbortSwap) => handle_abort_swap(event, runtime),
        _ => Ok(None),
    }
}

fn attempt_transition_to_init_maker(
    mut event: Event,
    runtime: &mut Runtime,
    swap_role: SwapRole,
) -> Result<Option<SwapStateMachine>, Error> {
    match event.request.clone() {
        BusMsg::Ctl(CtlMsg::MakeSwap(InitMakerSwap {
            peerd,
            report_to,
            key_manager,
            swap_id: _,
            target_bitcoin_address,
            target_monero_address,
            commit: remote_commit,
        })) => {
            // start watching block height changes
            runtime
                .syncer_state
                .watch_height(event.endpoints, Blockchain::Bitcoin)?;
            runtime
                .syncer_state
                .watch_height(event.endpoints, Blockchain::Monero)?;
            runtime.peer_service = peerd;
            if runtime.peer_service != ServiceId::Loopback {
                runtime.connected = true;
            }
            runtime.enquirer = Some(report_to);

            match swap_role {
                SwapRole::Bob => {
                    let remote_commit = if let Commit::AliceParameters(remote_commit) =
                        remote_commit
                    {
                        remote_commit
                    } else {
                        return Err(Error::Farcaster(
                            "Local swap role is Bob, but received Bob remote commit".to_string(),
                        ));
                    };
                    let mut swap_key_manager = BobSwapKeyManager::new_maker_bob(
                        &mut event,
                        runtime,
                        target_bitcoin_address,
                        target_monero_address,
                        key_manager.0,
                    )?;
                    let local_commit =
                        swap_key_manager
                            .maker_commit(&mut event, runtime)
                            .map_err(|err| {
                                runtime.report_failure(
                                    event.endpoints,
                                    Failure {
                                        code: FailureCode::Unknown,
                                        info: err.to_string(),
                                    },
                                )
                            })?;
                    // send maker commit message to counter-party
                    runtime.log_trace(format!("sending peer MakerCommit msg {}", &local_commit));
                    runtime.send_peer(
                        event.endpoints,
                        PeerMsg::MakerCommit(Commit::BobParameters(local_commit)),
                    )?;

                    Ok(Some(SwapStateMachine::BobInitMaker(BobInitMaker {
                        remote_commit,
                        swap_key_manager,
                    })))
                }
                SwapRole::Alice => {
                    let remote_commit = if let Commit::BobParameters(remote_commit) = remote_commit
                    {
                        remote_commit
                    } else {
                        return Err(Error::Farcaster(
                            "Local swap role is Bob, but received Bob remote commit".to_string(),
                        ));
                    };
                    let mut swap_key_manager = AliceSwapKeyManager::new_maker_alice(
                        runtime,
                        target_bitcoin_address,
                        target_monero_address,
                        key_manager.0,
                    )?;
                    let local_commit =
                        swap_key_manager
                            .maker_commit(&mut event, runtime)
                            .map_err(|err| {
                                runtime.report_failure(
                                    event.endpoints,
                                    Failure {
                                        code: FailureCode::Unknown,
                                        info: err.to_string(),
                                    },
                                )
                            })?;
                    // send maker commit message to counter-party
                    runtime.log_trace(format!("sending peer MakerCommit msg {}", &local_commit));
                    runtime.send_peer(
                        event.endpoints,
                        PeerMsg::MakerCommit(Commit::AliceParameters(local_commit)),
                    )?;

                    Ok(Some(SwapStateMachine::AliceInitMaker(AliceInitMaker {
                        remote_commit,
                        swap_key_manager,
                    })))
                }
            }
        }
        BusMsg::Ctl(CtlMsg::AbortSwap) => handle_abort_swap(event, runtime),
        _ => Ok(None),
    }
}

fn try_bob_init_taker_to_bob_taker_maker_commit(
    event: Event,
    runtime: &mut Runtime,
    bob_init_taker: BobInitTaker,
) -> Result<Option<SwapStateMachine>, Error> {
    let BobInitTaker {
        mut swap_key_manager,
    } = bob_init_taker;
    match event.request.clone() {
        BusMsg::P2p(PeerMsg::DealNotFound(_)) => {
            runtime.log_error(format!(
                "Taken deal {} was not found by the maker, aborting this swap.",
                runtime.deal.id().swap_id(),
            ));
            // just cancel the swap, no additional logic required
            handle_bob_abort_swap(event, runtime, swap_key_manager)
        }
        BusMsg::P2p(PeerMsg::MakerCommit(Commit::AliceParameters(remote_commit))) => {
            runtime.log_debug("Received remote maker commitment");
            let reveal = swap_key_manager.create_reveal_from_local_params(runtime)?;
            runtime.log_debug("swap_key_manager handled maker commit and produced reveal");
            runtime.send_peer(event.endpoints, PeerMsg::Reveal(reveal))?;
            runtime.log_trace("Sent reveal peer message to peerd");
            Ok(Some(SwapStateMachine::BobTakerMakerCommit(
                BobTakerMakerCommit {
                    remote_commit,
                    swap_key_manager,
                },
            )))
        }
        BusMsg::Ctl(CtlMsg::AbortSwap) => handle_bob_abort_swap(event, runtime, swap_key_manager),
        _ => Ok(None),
    }
}

fn try_alice_init_taker_to_alice_taker_maker_commit(
    event: Event,
    runtime: &mut Runtime,
    bob_init_taker: AliceInitTaker,
) -> Result<Option<SwapStateMachine>, Error> {
    let AliceInitTaker {
        mut swap_key_manager,
    } = bob_init_taker;
    match event.request {
        BusMsg::P2p(PeerMsg::DealNotFound(_)) => {
            runtime.log_error(format!(
                "Taken deal {} was not found by the maker, aborting this swap.",
                runtime.deal.id().swap_id(),
            ));
            // just cancel the swap, no additional logic required
            handle_abort_swap(event, runtime)
        }
        BusMsg::P2p(PeerMsg::MakerCommit(Commit::BobParameters(remote_commit))) => {
            runtime.log_debug("Received remote maker commitment");
            let reveal = swap_key_manager.create_reveal_from_local_params(runtime)?;
            runtime.log_debug("swap_key_manager handled maker commit and produced reveal");
            runtime.send_peer(event.endpoints, PeerMsg::Reveal(reveal))?;
            runtime.log_info("Sent reveal peer message to peerd");
            Ok(Some(SwapStateMachine::AliceTakerMakerCommit(
                AliceTakerMakerCommit {
                    remote_commit,
                    swap_key_manager,
                },
            )))
        }
        BusMsg::Ctl(CtlMsg::AbortSwap) => handle_abort_swap(event, runtime),
        _ => Ok(None),
    }
}

fn try_bob_taker_maker_commit_to_bob_reveal(
    event: Event,
    runtime: &mut Runtime,
    bob_taker_maker_commit: BobTakerMakerCommit,
) -> Result<Option<SwapStateMachine>, Error> {
    let BobTakerMakerCommit {
        swap_key_manager,
        remote_commit,
    } = bob_taker_maker_commit;
    attempt_transition_to_bob_reveal(event, runtime, remote_commit, swap_key_manager)
}

fn try_alice_taker_maker_commit_to_alice_reveal(
    event: Event,
    runtime: &mut Runtime,
    alice_taker_maker_commit: AliceTakerMakerCommit,
) -> Result<Option<SwapStateMachine>, Error> {
    let AliceTakerMakerCommit {
        remote_commit,
        swap_key_manager,
    } = alice_taker_maker_commit;
    attempt_transition_to_alice_reveal(event, runtime, remote_commit, swap_key_manager)
}

fn try_bob_init_maker_to_bob_reveal(
    event: Event,
    runtime: &mut Runtime,
    bob_init_maker: BobInitMaker,
) -> Result<Option<SwapStateMachine>, Error> {
    let BobInitMaker {
        swap_key_manager,
        remote_commit,
    } = bob_init_maker;
    attempt_transition_to_bob_reveal(event, runtime, remote_commit, swap_key_manager)
}

fn try_alice_init_maker_to_alice_reveal(
    event: Event,
    runtime: &mut Runtime,
    alice_init_maker: AliceInitMaker,
) -> Result<Option<SwapStateMachine>, Error> {
    let AliceInitMaker {
        remote_commit,
        swap_key_manager,
    } = alice_init_maker;
    attempt_transition_to_alice_reveal(event, runtime, remote_commit, swap_key_manager)
}

fn try_bob_reveal_to_bob_fee_estimated(
    mut event: Event,
    runtime: &mut Runtime,
    bob_reveal: BobReveal,
) -> Result<Option<SwapStateMachine>, Error> {
    let BobReveal {
        remote_params,
        swap_key_manager,
    } = bob_reveal;
    match &event.request {
        BusMsg::Sync(SyncMsg::Event(SyncEvent::FeeEstimation(FeeEstimation {
            fee_estimations:
                FeeEstimations::BitcoinFeeEstimation {
                    high_priority_sats_per_kvbyte,
                    ..
                },
            ..
        }))) => {
            // FIXME handle low priority as well
            runtime.log_info(format!("Fee: {} sat/kvB", high_priority_sats_per_kvbyte));
            runtime.log_debug("Sending funding info to farcasterd");
            let funding_address = swap_key_manager
                .funding_address()
                .expect("Am Bob, so have funding address");
            let required_funding_amount = runtime.ask_bob_to_fund(
                *high_priority_sats_per_kvbyte,
                funding_address.clone(),
                event.endpoints,
            )?;

            runtime.log_debug(format!("Watch arbitrating funding {}", funding_address));
            let watch_addr_task = runtime
                .syncer_state
                .watch_addr_btc(funding_address, TxLabel::Funding);
            event.send_sync_service(
                runtime.syncer_state.bitcoin_syncer(),
                SyncMsg::Task(watch_addr_task),
            )?;
            Ok(Some(SwapStateMachine::BobFeeEstimated(BobFeeEstimated {
                remote_params,
                swap_key_manager,
                required_funding_amount,
            })))
        }
        BusMsg::Ctl(CtlMsg::AbortSwap) => handle_bob_abort_swap(event, runtime, swap_key_manager),
        _ => Ok(None),
    }
}

fn try_bob_fee_estimated_to_bob_funded(
    mut event: Event,
    runtime: &mut Runtime,
    bob_reveal: BobFeeEstimated,
) -> Result<Option<SwapStateMachine>, Error> {
    let BobFeeEstimated {
        remote_params,
        mut swap_key_manager,
        required_funding_amount,
    } = bob_reveal;
    match &event.request {
        BusMsg::Sync(SyncMsg::Event(SyncEvent::AddressTransaction(AddressTransaction {
            id,
            amount,
            tx,
            ..
        }))) if runtime.syncer_state.tasks.watched_addrs.get(id) == Some(&TxLabel::Funding)
            && runtime.syncer_state.awaiting_funding =>
        {
            let tx = bitcoin::Transaction::deserialize(
                &tx.iter().flatten().copied().collect::<Vec<u8>>(),
            )?;
            runtime.log_info(format!(
                "Received AddressTransaction, processing tx {}",
                &tx.txid().tx_hash()
            ));
            log_tx_seen(runtime.swap_id, &TxLabel::Funding, &tx.txid().into());
            runtime.syncer_state.awaiting_funding = false;
            // If the bitcoin amount does not match the expected funding amount, abort the swap
            let amount = bitcoin::Amount::from_sat(*amount);
            // Abort the swap in case of bad funding amount
            if amount != required_funding_amount {
                // incorrect funding, start aborting procedure
                let msg = format!("Incorrect amount funded. Required: {}, Funded: {}. Do not fund this swap anymore, will abort and atttempt to sweep the Bitcoin to the provided address.", amount, required_funding_amount);
                runtime.log_error(&msg);
                runtime.report_progress_message(event.endpoints, msg)?;
                return handle_bob_abort_swap(event, runtime, swap_key_manager);
            } else {
                // funding completed, amount is correct
                event.send_ctl_service(
                    ServiceId::Farcasterd,
                    CtlMsg::FundingCompleted(Blockchain::Bitcoin),
                )?;
            }

            // process tx with swap_key_manager
            swap_key_manager.process_funding_tx(runtime, Tx::Funding(tx))?;
            let core_arbitrating_setup =
                swap_key_manager.create_core_arb(runtime, &remote_params)?;

            // register a watch task for arb lock, cancel, and refund
            for (&tx, tx_label) in [
                &core_arbitrating_setup.lock,
                &core_arbitrating_setup.cancel,
                &core_arbitrating_setup.refund,
            ]
            .iter()
            .zip([TxLabel::Lock, TxLabel::Cancel, TxLabel::Refund])
            {
                runtime.log_debug(format!("register watch {} tx", tx_label.label()));
                let txid = tx.clone().extract_tx().txid();
                let task = runtime.syncer_state.watch_tx_btc(txid, tx_label);
                event.send_sync_service(
                    runtime.syncer_state.bitcoin_syncer(),
                    SyncMsg::Task(task),
                )?;
            }

            // Set the monero address creation height for Bob before setting the first checkpoint
            if runtime.acc_lock_height_lower_bound.is_none() {
                runtime.acc_lock_height_lower_bound =
                    Some(runtime.temporal_safety.block_height_reorg_lower_bound(
                        Blockchain::Monero,
                        runtime.syncer_state.height(Blockchain::Monero),
                    ));
            }

            // checkpoint swap pre lock bob
            runtime.log_debug("checkpointing bob pre lock state");
            // transition to new state
            let new_ssm = SwapStateMachine::BobFunded(BobFunded {
                remote_params,
                core_arbitrating_setup: core_arbitrating_setup.clone(),
                swap_key_manager,
            });
            runtime.checkpoint_state(
                event.endpoints,
                Some(PeerMsg::CoreArbitratingSetup(
                    core_arbitrating_setup.clone(),
                )),
                new_ssm.clone(),
            )?;

            // send the message to counter-party
            runtime.log_debug("sending core arb setup to peer");
            runtime.send_peer(
                event.endpoints,
                PeerMsg::CoreArbitratingSetup(core_arbitrating_setup),
            )?;
            Ok(Some(new_ssm))
        }
        BusMsg::Ctl(CtlMsg::AbortSwap) => handle_bob_abort_swap(event, runtime, swap_key_manager),
        _ => Ok(None),
    }
}

fn try_bob_funded_to_bob_refund_procedure_signature(
    mut event: Event,
    runtime: &mut Runtime,
    bob_funded: BobFunded,
) -> Result<Option<SwapStateMachine>, Error> {
    let BobFunded {
        remote_params,
        core_arbitrating_setup,
        mut swap_key_manager,
    } = bob_funded;
    match &event.request {
        BusMsg::P2p(PeerMsg::RefundProcedureSignatures(refund_proc)) => {
            runtime.log_debug("Processing refund proc sig with swap_key_manager.");
            let HandleRefundProcedureSignaturesRes {
                buy_procedure_signature,
                lock_tx,
                cancel_tx,
                refund_tx,
            } = swap_key_manager.handle_refund_procedure_signatures(
                runtime,
                refund_proc.clone(),
                &remote_params,
                core_arbitrating_setup,
            )?;
            // Process and broadcast lock tx
            log_tx_created(runtime.swap_id, TxLabel::Lock);
            // Process params, aggregate and watch xmr address
            let (spend, view) =
                aggregate_xmr_spend_view(&remote_params, &swap_key_manager.local_params());
            let address = monero::Address::from_viewpair(
                runtime.syncer_state.network.into(),
                &ViewPair { spend, view },
            );
            let txlabel = TxLabel::AccLock;
            let task = runtime.syncer_state.watch_addr_xmr(
                address,
                view,
                txlabel,
                runtime.acc_lock_height_lower_bound.unwrap_or_else(|| {
                    runtime.temporal_safety.block_height_reorg_lower_bound(
                        Blockchain::Monero,
                        runtime.syncer_state.height(Blockchain::Monero),
                    )
                }),
            );

            event.send_sync_service(runtime.syncer_state.monero_syncer(), SyncMsg::Task(task))?;
            // Handle Cancel and Refund transaction
            log_tx_created(runtime.swap_id, TxLabel::Cancel);
            log_tx_created(runtime.swap_id, TxLabel::Refund);
            runtime.txs.insert(TxLabel::Cancel, cancel_tx);
            runtime.txs.insert(TxLabel::Refund, refund_tx);
            // register a watch task for buy tx.
            // registration performed now already to ensure it's present in checkpoint.
            runtime.log_debug("register watch buy tx task");
            let buy_tx = buy_procedure_signature.buy.clone().extract_tx();
            let task = runtime
                .syncer_state
                .watch_tx_btc(buy_tx.txid(), TxLabel::Buy);
            event.send_sync_service(runtime.syncer_state.bitcoin_syncer(), SyncMsg::Task(task))?;
            // Checkpoint BobRefundProcedureSignatures
            let new_ssm =
                SwapStateMachine::BobRefundProcedureSignatures(BobRefundProcedureSignatures {
                    remote_params,
                    swap_key_manager,
                    buy_procedure_signature,
                });
            runtime.log_debug("Checkpointing bob refund signature swapd state.");
            // manually add lock_tx to pending broadcasts to ensure it's checkpointed
            runtime
                .syncer_state
                .broadcast(lock_tx.clone(), TxLabel::Lock);
            runtime.checkpoint_state(event.endpoints, None, new_ssm.clone())?;
            runtime.broadcast(lock_tx, TxLabel::Lock, event.endpoints)?;
            Ok(Some(new_ssm))
        }
        BusMsg::Ctl(CtlMsg::AbortSwap) => handle_bob_abort_swap(event, runtime, swap_key_manager),
        _ => Ok(None),
    }
}

fn try_bob_refund_procedure_signatures_to_bob_accordant_lock(
    mut event: Event,
    runtime: &mut Runtime,
    bob_refund_procedure_signatures: BobRefundProcedureSignatures,
) -> Result<Option<SwapStateMachine>, Error> {
    let BobRefundProcedureSignatures {
        remote_params,
        swap_key_manager,
        buy_procedure_signature,
    } = bob_refund_procedure_signatures;
    match &event.request {
        BusMsg::Sync(SyncMsg::Event(SyncEvent::AddressTransaction(AddressTransaction {
            id,
            hash,
            amount,
            block: _,
            tx: _,
            incoming,
        }))) if runtime.syncer_state.tasks.watched_addrs.contains_key(id)
            && runtime.syncer_state.is_watched_addr(&TxLabel::AccLock)
            && runtime.syncer_state.tasks.watched_addrs.get(id) == Some(&TxLabel::AccLock)
            && *incoming =>
        {
            let amount = monero::Amount::from_pico(*amount);
            if amount < runtime.deal.parameters.accordant_amount {
                runtime.log_warn(format!(
                    "Not enough monero locked: expected {}, found {}",
                    runtime.deal.parameters.accordant_amount, amount
                ));
                return Ok(None);
            }
            if let Some(tx_label) = runtime.syncer_state.tasks.watched_addrs.remove(id) {
                let abort_task = runtime.syncer_state.abort_task(*id);
                let watch_tx = runtime.syncer_state.watch_tx_xmr(*hash, tx_label);
                event.send_sync_service(
                    runtime.syncer_state.monero_syncer(),
                    SyncMsg::Task(watch_tx),
                )?;
                event.send_sync_service(
                    runtime.syncer_state.monero_syncer(),
                    SyncMsg::Task(abort_task),
                )?;
            }
            Ok(Some(SwapStateMachine::BobAccordantLock(BobAccordantLock {
                remote_params,
                swap_key_manager,
                buy_procedure_signature,
            })))
        }
        _ => handle_bob_swap_interrupt_after_lock(event, runtime),
    }
}

fn try_bob_accordant_lock_to_bob_accordant_lock_final(
    event: Event,
    runtime: &mut Runtime,
    bob_accordant_lock: BobAccordantLock,
) -> Result<Option<SwapStateMachine>, Error> {
    let BobAccordantLock {
        remote_params,
        swap_key_manager,
        buy_procedure_signature,
    } = bob_accordant_lock;
    match event.request {
        BusMsg::Sync(SyncMsg::Event(SyncEvent::TransactionConfirmations(
            TransactionConfirmations {
                id,
                confirmations: Some(confirmations),
                ..
            },
        ))) if runtime
            .temporal_safety
            .final_tx(confirmations, Blockchain::Monero)
            && runtime.syncer_state.tasks.watched_txs.get(&id) == Some(&TxLabel::AccLock) =>
        {
            runtime.send_peer(
                event.endpoints,
                PeerMsg::BuyProcedureSignature(buy_procedure_signature.clone()),
            )?;
            Ok(Some(SwapStateMachine::BobAccordantLockFinal(
                BobAccordantLockFinal {
                    remote_params,
                    buy_procedure_signature,
                    swap_key_manager,
                },
            )))
        }
        _ => handle_bob_swap_interrupt_after_lock(event, runtime),
    }
}

fn try_bob_accordant_lock_final_to_bob_buy_seen(
    mut event: Event,
    runtime: &mut Runtime,
    bob_accordant_lock_final: BobAccordantLockFinal,
) -> Result<Option<SwapStateMachine>, Error> {
    let BobAccordantLockFinal {
        remote_params,
        buy_procedure_signature,
        mut swap_key_manager,
    } = bob_accordant_lock_final;
    match event.request.clone() {
        BusMsg::Sync(SyncMsg::Event(SyncEvent::TransactionConfirmations(
            TransactionConfirmations {
                id,
                confirmations: Some(_),
                ref tx,
                ..
            },
        ))) if runtime.syncer_state.tasks.watched_txs.get(&id) == Some(&TxLabel::Buy)
            && runtime.syncer_state.tasks.txids.contains_key(&TxLabel::Buy) =>
        {
            runtime.log_warn(
                "Peerd might crash, just ignore it, counterparty closed \
                    connection, because they are done with the swap, but you don't need it anymore either!"
            );
            let tx = bitcoin::Transaction::deserialize(
                &tx.iter().flatten().copied().collect::<Vec<u8>>(),
            )?;

            log_tx_seen(runtime.swap_id, &TxLabel::Buy, &tx.txid().into());
            let sweep_xmr = swap_key_manager.process_buy_tx(
                runtime,
                tx,
                &mut event,
                remote_params,
                buy_procedure_signature,
            )?;
            let task = runtime.syncer_state.sweep_xmr(sweep_xmr.clone(), true);
            runtime.syncer_state.tasks.txids.remove_entry(&TxLabel::Buy);

            let sweep_address = if let Task::SweepAddress(sweep_address) = task {
                sweep_address
            } else {
                return Ok(None);
            };
            runtime.log_monero_maturity(sweep_xmr.destination_address);
            Ok(Some(SwapStateMachine::BobBuySeen(sweep_address)))
        }
        _ => handle_bob_swap_interrupt_after_lock(event, runtime),
    }
}

fn try_bob_buy_seen_to_bob_buy_sweeping(
    mut event: Event,
    runtime: &mut Runtime,
    task: SweepAddress,
) -> Result<Option<SwapStateMachine>, Error> {
    match event.request {
        BusMsg::Sync(SyncMsg::Event(SyncEvent::TransactionConfirmations(
            TransactionConfirmations {
                confirmations: Some(confirmations),
                ..
            },
        ))) if confirmations >= runtime.temporal_safety.sweep_monero_thr => {
            // safe cast
            let request = SyncMsg::Task(Task::SweepAddress(task));
            runtime.log_info(format!(
                "Monero are spendable now (height {}), sweeping ephemeral swap_key_manager",
                runtime.syncer_state.monero_height.label()
            ));
            event.send_sync_service(runtime.syncer_state.monero_syncer(), request)?;
            Ok(Some(SwapStateMachine::BobBuySweeping))
        }
        _ => Ok(None),
    }
}

fn try_bob_cancel_final_to_swap_end(
    mut event: Event,
    runtime: &mut Runtime,
) -> Result<Option<SwapStateMachine>, Error> {
    match event.request {
        BusMsg::Sync(SyncMsg::Event(SyncEvent::TransactionConfirmations(
            TransactionConfirmations {
                id,
                confirmations: Some(confirmations),
                ..
            },
        ))) if runtime
            .temporal_safety
            .final_tx(confirmations, Blockchain::Bitcoin) =>
        {
            match runtime.syncer_state.tasks.watched_txs.get(&id) {
                Some(&TxLabel::Refund) => {
                    let abort_all = Task::Abort(Abort {
                        task_target: TaskTarget::AllTasks,
                        respond: Boolean::False,
                    });
                    event.send_sync_service(
                        runtime.syncer_state.monero_syncer(),
                        SyncMsg::Task(abort_all.clone()),
                    )?;
                    event.send_sync_service(
                        runtime.syncer_state.bitcoin_syncer(),
                        SyncMsg::Task(abort_all),
                    )?;
                    // remove txs to invalidate outdated states
                    runtime.txs.remove(&TxLabel::Cancel);
                    runtime.txs.remove(&TxLabel::Refund);
                    runtime.txs.remove(&TxLabel::Buy);
                    runtime.txs.remove(&TxLabel::Punish);
                    // send swap outcome to farcasterd
                    Ok(Some(SwapStateMachine::SwapEnd(Outcome::FailureRefund)))
                }
                Some(&TxLabel::Punish) => {
                    let abort_all = Task::Abort(Abort {
                        task_target: TaskTarget::AllTasks,
                        respond: Boolean::False,
                    });
                    event.send_sync_service(
                        runtime.syncer_state.monero_syncer(),
                        SyncMsg::Task(abort_all.clone()),
                    )?;
                    event.send_sync_service(
                        runtime.syncer_state.bitcoin_syncer(),
                        SyncMsg::Task(abort_all),
                    )?;
                    // remove txs to invalidate outdated states
                    runtime.txs.remove(&TxLabel::Cancel);
                    runtime.txs.remove(&TxLabel::Refund);
                    runtime.txs.remove(&TxLabel::Buy);
                    Ok(Some(SwapStateMachine::SwapEnd(Outcome::FailurePunish)))
                }
                _ => Ok(None),
            }
        }

        BusMsg::Sync(SyncMsg::Event(SyncEvent::AddressTransaction(AddressTransaction {
            id,
            hash: Txid::Bitcoin(ref hash),
            incoming,
            ..
        }))) if runtime.syncer_state.tasks.watched_addrs.get(&id) == Some(&TxLabel::Cancel)
            && !incoming =>
        {
            let tasks = &runtime.syncer_state.tasks.txids;
            debug_assert!(tasks.get(&TxLabel::Refund).is_some());
            if Some(hash) != tasks.get(&TxLabel::Refund)
                && Some(hash) != tasks.get(&TxLabel::Punish)
            {
                let watch_punish_task = runtime.syncer_state.watch_tx_btc(*hash, TxLabel::Punish);
                event.send_sync_service(
                    runtime.syncer_state.bitcoin_syncer(),
                    SyncMsg::Task(watch_punish_task),
                )?;
            }
            Ok(None)
        }

        _ => Ok(None),
    }
}

fn try_bob_canceled_to_bob_cancel_final(
    event: Event,
    runtime: &mut Runtime,
) -> Result<Option<SwapStateMachine>, Error> {
    match event.request {
        // If Cancel Broadcast failed, then we need to go into Buy
        BusMsg::Sync(SyncMsg::Event(SyncEvent::TransactionConfirmations(
            TransactionConfirmations {
                id,
                confirmations: Some(confirmations),
                ..
            },
        ))) if runtime
            .temporal_safety
            .final_tx(confirmations, Blockchain::Bitcoin)
            && runtime.syncer_state.tasks.watched_txs.get(&id) == Some(&TxLabel::Cancel)
            && runtime.txs.contains_key(&TxLabel::Refund) =>
        {
            runtime.log_trace("Bob publishes refund tx");
            if !runtime.temporal_safety.safe_refund(confirmations) {
                runtime.log_warn("Publishing refund tx, but we might already have been punished");
            }
            let (tx_label, refund_tx) = runtime.txs.remove_entry(&TxLabel::Refund).unwrap();
            runtime.broadcast(refund_tx, tx_label, event.endpoints)?;
            Ok(Some(SwapStateMachine::BobCancelFinal))
        }
        _ => Ok(None),
    }
}

fn try_awaiting_sweep_to_swap_end(
    mut event: Event,
    runtime: &mut Runtime,
) -> Result<Option<SwapStateMachine>, Error> {
    match event.request {
        BusMsg::Sync(SyncMsg::Event(SyncEvent::TaskAborted(TaskAborted { ref id, .. })))
            if id.len() == 1 && runtime.syncer_state.tasks.sweeping_addr == Some(id[0]) =>
        {
            event.send_client_ctl(
                ServiceId::Farcasterd,
                CtlMsg::FundingCanceled(Blockchain::Bitcoin),
            )?;
            runtime.log_info("Aborted swap.");
            Ok(Some(SwapStateMachine::SwapEnd(Outcome::FailureAbort)))
        }

        BusMsg::Sync(SyncMsg::Event(SyncEvent::SweepSuccess(SweepSuccess { id, .. })))
            if runtime.syncer_state.tasks.sweeping_addr == Some(id) =>
        {
            event.send_client_ctl(
                ServiceId::Farcasterd,
                CtlMsg::FundingCanceled(Blockchain::Bitcoin),
            )?;
            runtime.log_info("Aborted swap.");
            Ok(Some(SwapStateMachine::SwapEnd(Outcome::FailureAbort)))
        }
        _ => Ok(None),
    }
}

fn try_alice_reveal_to_alice_core_arbitrating_setup(
    mut event: Event,
    runtime: &mut Runtime,
    alice_reveal: AliceReveal,
) -> Result<Option<SwapStateMachine>, Error> {
    let AliceReveal {
        remote_params,
        mut swap_key_manager,
    } = alice_reveal;
    match event.request.clone() {
        BusMsg::P2p(PeerMsg::CoreArbitratingSetup(setup)) => {
            // register a watch task for arb lock, cancel, and refund
            for (&tx, tx_label) in [&setup.lock, &setup.cancel, &setup.refund].iter().zip([
                TxLabel::Lock,
                TxLabel::Cancel,
                TxLabel::Refund,
            ]) {
                runtime.log_debug(format!("Register watch {} tx", tx_label));
                let txid = tx.clone().extract_tx().txid();
                let task = runtime.syncer_state.watch_tx_btc(txid, tx_label);
                event.send_sync_service(
                    runtime.syncer_state.bitcoin_syncer(),
                    SyncMsg::Task(task),
                )?;
            }
            // handle the core arbitrating setup message with the swap_key_manager
            runtime.log_debug("Handling core arb setup with swap_key_manager");
            let HandleCoreArbitratingSetupRes {
                refund_procedure_signatures,
                cancel_tx,
                punish_tx,
                alice_cancel_signature,
                adaptor_refund,
            } = swap_key_manager.handle_core_arbitrating_setup(
                runtime,
                setup.clone(),
                &remote_params,
            )?;
            // handle Cancel and Punish transactions
            log_tx_created(runtime.swap_id, TxLabel::Cancel);
            runtime.txs.insert(TxLabel::Cancel, cancel_tx);
            log_tx_created(runtime.swap_id, TxLabel::Punish);
            runtime.txs.insert(TxLabel::Punish, punish_tx);
            // checkpoint alice pre lock bob
            let new_ssm = SwapStateMachine::AliceCoreArbitratingSetup(AliceCoreArbitratingSetup {
                remote_params,
                core_arbitrating_setup: setup,
                alice_cancel_signature,
                adaptor_refund,
                swap_key_manager,
            });
            runtime.log_debug("checkpointing alice pre lock state");
            runtime.checkpoint_state(
                event.endpoints,
                Some(PeerMsg::RefundProcedureSignatures(
                    refund_procedure_signatures.clone(),
                )),
                new_ssm.clone(),
            )?;
            // send refund procedure signature message to counter-party
            runtime.log_debug("sending refund proc sig to peer");
            runtime.send_peer(
                event.endpoints,
                PeerMsg::RefundProcedureSignatures(refund_procedure_signatures),
            )?;
            Ok(Some(new_ssm))
        }
        BusMsg::Ctl(CtlMsg::AbortSwap) => handle_abort_swap(event, runtime),
        _ => Ok(None),
    }
}

fn try_alice_core_arbitrating_setup_to_alice_arbitrating_lock_final(
    mut event: Event,
    runtime: &mut Runtime,
    alice_core_arbitrating_setup: AliceCoreArbitratingSetup,
) -> Result<Option<SwapStateMachine>, Error> {
    let AliceCoreArbitratingSetup {
        remote_params,
        core_arbitrating_setup,
        alice_cancel_signature,
        adaptor_refund,
        swap_key_manager,
    } = alice_core_arbitrating_setup;
    match event.request {
        BusMsg::Sync(SyncMsg::Event(SyncEvent::TransactionConfirmations(
            TransactionConfirmations {
                id,
                confirmations: Some(confirmations),
                ..
            },
        ))) if runtime
            .temporal_safety
            .final_tx(confirmations, Blockchain::Bitcoin)
            && runtime.syncer_state.tasks.watched_txs.get(&id) == Some(&TxLabel::Lock) =>
        {
            let (spend, view) =
                aggregate_xmr_spend_view(&swap_key_manager.local_params(), &remote_params);
            // Set the monero address creation height for Alice right after the first aggregation
            if runtime.acc_lock_height_lower_bound.is_none() {
                runtime.acc_lock_height_lower_bound =
                    Some(runtime.temporal_safety.block_height_reorg_lower_bound(
                        Blockchain::Monero,
                        runtime.syncer_state.height(Blockchain::Monero),
                    ));
            }
            let viewpair = ViewPair { spend, view };
            let address =
                monero::Address::from_viewpair(runtime.syncer_state.network.into(), &viewpair);
            let swap_id = runtime.swap_id();
            let amount = runtime.deal.parameters.accordant_amount;
            let funding_info = MoneroFundingInfo {
                swap_id,
                address,
                amount,
            };
            let txlabel = TxLabel::AccLock;
            let watch_addr_task = runtime.syncer_state.watch_addr_xmr(
                address,
                view,
                txlabel,
                runtime.acc_lock_height_lower_bound.unwrap_or_else(|| {
                    runtime.temporal_safety.block_height_reorg_lower_bound(
                        Blockchain::Monero,
                        runtime.syncer_state.height(Blockchain::Monero),
                    )
                }),
            );
            event.send_sync_service(
                runtime.syncer_state.monero_syncer(),
                SyncMsg::Task(watch_addr_task),
            )?;
            Ok(Some(SwapStateMachine::AliceArbitratingLockFinal(
                AliceArbitratingLockFinal {
                    swap_key_manager,
                    required_funding_amount: amount,
                    funding_info,
                    remote_params,
                    core_arbitrating_setup,
                    alice_cancel_signature,
                    adaptor_refund,
                },
            )))
        }
        _ => handle_alice_swap_interrupt_after_lock(
            event,
            runtime,
            remote_params,
            adaptor_refund,
            swap_key_manager,
        ),
    }
}

fn try_alice_arbitrating_lock_final_to_alice_accordant_lock(
    mut event: Event,
    runtime: &mut Runtime,
    alice_arbitrating_lock_final: AliceArbitratingLockFinal,
) -> Result<Option<SwapStateMachine>, Error> {
    let AliceArbitratingLockFinal {
        swap_key_manager,
        funding_info,
        required_funding_amount,
        remote_params,
        core_arbitrating_setup,
        alice_cancel_signature,
        adaptor_refund,
    } = alice_arbitrating_lock_final;
    match event.request {
        BusMsg::Sync(SyncMsg::Event(SyncEvent::Empty(id)))
            if runtime.syncer_state.tasks.watched_addrs.get(&id) == Some(&TxLabel::AccLock) =>
        {
            runtime.log_info(format!(
                "Send {} to {}",
                funding_info.amount.bright_green_bold(),
                funding_info.address.addr(),
            ));
            runtime.syncer_state.awaiting_funding = true;
            if let Some(enquirer) = runtime.enquirer.clone() {
                event.send_ctl_service(
                    enquirer,
                    CtlMsg::FundingInfo(FundingInfo::Monero(funding_info.clone())),
                )?;
            }
            Ok(Some(SwapStateMachine::AliceArbitratingLockFinal(
                AliceArbitratingLockFinal {
                    swap_key_manager,
                    funding_info,
                    required_funding_amount,
                    remote_params,
                    core_arbitrating_setup,
                    alice_cancel_signature,
                    adaptor_refund,
                },
            )))
        }

        // warn user about funding if we're close to cancel becoming valid,
        // and remain in AliceArbitratingLockFinal state
        BusMsg::Sync(SyncMsg::Event(SyncEvent::TransactionConfirmations(
            TransactionConfirmations {
                id,
                confirmations: Some(confirmations),
                ..
            },
        ))) if runtime
            .temporal_safety
            .final_tx(confirmations, Blockchain::Bitcoin)
            && runtime.syncer_state.tasks.watched_txs.get(&id) == Some(&TxLabel::Lock)
            && runtime
                .temporal_safety
                .stop_funding_before_cancel(confirmations)
            && runtime.syncer_state.awaiting_funding =>
        {
            runtime.log_warn("Alice, the swap may be cancelled soon. Do not fund anymore");
            event.complete_ctl_service(
                ServiceId::Farcasterd,
                CtlMsg::FundingCanceled(Blockchain::Monero),
            )?;
            runtime.syncer_state.awaiting_funding = false;
            Ok(Some(SwapStateMachine::AliceArbitratingLockFinal(
                AliceArbitratingLockFinal {
                    swap_key_manager,
                    funding_info,
                    required_funding_amount,
                    remote_params,
                    core_arbitrating_setup,
                    alice_cancel_signature,
                    adaptor_refund,
                },
            )))
        }

        BusMsg::Sync(SyncMsg::Event(SyncEvent::AddressTransaction(AddressTransaction {
            id,
            ref hash,
            amount,
            ref block,
            ref tx,
            incoming,
        }))) if runtime.syncer_state.tasks.watched_addrs.get(&id) == Some(&TxLabel::AccLock)
            && incoming =>
        {
            runtime.log_debug(format!(
                "Event details: {} {:?} {} {:?} {:?}",
                id, hash, amount, block, tx
            ));
            let txlabel = TxLabel::AccLock;
            let task = runtime.syncer_state.watch_tx_xmr(*hash, txlabel);
            if runtime.syncer_state.awaiting_funding {
                event.send_ctl_service(
                    ServiceId::Farcasterd,
                    CtlMsg::FundingCompleted(Blockchain::Monero),
                )?;
                runtime.syncer_state.awaiting_funding = false;
            }
            event.send_sync_service(runtime.syncer_state.monero_syncer(), SyncMsg::Task(task))?;
            if runtime
                .syncer_state
                .tasks
                .watched_addrs
                .remove(&id)
                .is_some()
            {
                let abort_task = runtime.syncer_state.abort_task(id);
                event.send_sync_service(
                    runtime.syncer_state.monero_syncer(),
                    SyncMsg::Task(abort_task),
                )?;
            }

            match amount.cmp(&required_funding_amount.as_pico()) {
                // Underfunding
                Ordering::Less => {
                    // Alice still views underfunding as valid in the hope that Bob still passes her BuyProcSig
                    let msg = format!(
                                    "Too small amount funded. Required: {}, Funded: {}. Do not fund this swap anymore, will attempt to refund.",
                                    required_funding_amount,
                                    monero::Amount::from_pico(amount)
                                );
                    runtime.log_error(&msg);
                    runtime.report_progress_message(event.endpoints, msg)?;
                }
                // Overfunding
                Ordering::Greater => {
                    // Alice overfunded. To ensure that she does not publish the buy transaction
                    // if Bob gives her the BuySig, go straight to AliceCanceled
                    let msg = format!(
                                    "Too big amount funded. Required: {}, Funded: {}. Do not fund this swap anymore, will attempt to refund.",
                                    required_funding_amount,
                                    monero::Amount::from_pico(amount)
                                );
                    runtime.log_error(&msg);
                    runtime.report_progress_message(event.endpoints, msg)?;
                    // Alice moves on to AliceCanceled despite not broadcasting the cancel transaction.
                    return Ok(Some(SwapStateMachine::AliceCanceled(AliceCanceled {
                        remote_params,
                        adaptor_refund,
                        swap_key_manager,
                    })));
                }
                // Funding Exact
                Ordering::Equal => {}
            }

            Ok(Some(SwapStateMachine::AliceAccordantLock(
                AliceAccordantLock {
                    remote_params,
                    core_arbitrating_setup,
                    alice_cancel_signature,
                    adaptor_refund,
                    swap_key_manager,
                },
            )))
        }
        _ => handle_alice_swap_interrupt_after_lock(
            event,
            runtime,
            remote_params,
            adaptor_refund,
            swap_key_manager,
        ),
    }
}

fn try_alice_accordant_lock_to_alice_buy_procedure_signature(
    mut event: Event,
    runtime: &mut Runtime,
    alice_accordant_lock: AliceAccordantLock,
) -> Result<Option<SwapStateMachine>, Error> {
    let AliceAccordantLock {
        remote_params,
        core_arbitrating_setup,
        alice_cancel_signature,
        adaptor_refund,
        mut swap_key_manager,
    } = alice_accordant_lock;

    match event.request.clone() {
        BusMsg::P2p(PeerMsg::BuyProcedureSignature(buy_procedure_signature)) => {
            // register a watch task for buy
            runtime.log_debug("Registering watch buy tx task");
            let txid = buy_procedure_signature.buy.clone().extract_tx().txid();
            let task = runtime.syncer_state.watch_tx_btc(txid, TxLabel::Buy);
            event.send_sync_service(runtime.syncer_state.bitcoin_syncer(), SyncMsg::Task(task))?;
            // Handle the received buy procedure signature message with the swap_key_manager
            runtime.log_debug("Handling buy procedure signature with swap_key_manager");
            let HandleBuyProcedureSignatureRes { cancel_tx, buy_tx } = swap_key_manager
                .handle_buy_procedure_signature(
                    runtime,
                    buy_procedure_signature,
                    &remote_params,
                    core_arbitrating_setup,
                    alice_cancel_signature,
                )?;

            // Handle Cancel and Buy transactions
            log_tx_created(runtime.swap_id, TxLabel::Cancel);
            log_tx_created(runtime.swap_id, TxLabel::Buy);

            // Insert transactions into the runtime
            runtime.txs.insert(TxLabel::Cancel, cancel_tx.clone());
            runtime.txs.insert(TxLabel::Buy, buy_tx.clone());

            // Check if we should cancel the swap
            if let Some(SyncMsg::Event(SyncEvent::TransactionConfirmations(
                TransactionConfirmations {
                    confirmations: Some(confirmations),
                    ..
                },
            ))) = runtime.syncer_state.last_tx_event.get(&TxLabel::Lock)
            {
                if runtime.temporal_safety.valid_cancel(*confirmations) {
                    runtime.broadcast(cancel_tx, TxLabel::Cancel, event.endpoints)?;
                    return Ok(Some(SwapStateMachine::AliceCanceled(AliceCanceled {
                        remote_params,
                        adaptor_refund,
                        swap_key_manager,
                    })));
                }
            }

            // Broadcast the Buy transaction
            runtime.broadcast(buy_tx, TxLabel::Buy, event.endpoints)?;

            // checkpoint swap alice pre buy
            let new_ssm = SwapStateMachine::AliceBuyProcedureSignature;
            runtime.log_debug("checkpointing alice pre buy swapd state");
            runtime.checkpoint_state(event.endpoints, None, new_ssm.clone())?;
            Ok(Some(new_ssm))
        }
        _ => handle_alice_swap_interrupt_after_lock(
            event,
            runtime,
            remote_params,
            adaptor_refund,
            swap_key_manager,
        ),
    }
}

fn try_alice_buy_procedure_signature_to_swap_end(
    mut event: Event,
    runtime: &mut Runtime,
) -> Result<Option<SwapStateMachine>, Error> {
    match event.request {
        BusMsg::Sync(SyncMsg::Event(SyncEvent::TransactionConfirmations(
            TransactionConfirmations {
                id,
                confirmations: Some(confirmations),
                ..
            },
        ))) if runtime
            .temporal_safety
            .final_tx(confirmations, Blockchain::Bitcoin)
            && runtime.syncer_state.tasks.watched_txs.get(&id) == Some(&TxLabel::Buy) =>
        {
            let abort_all = Task::Abort(Abort {
                task_target: TaskTarget::AllTasks,
                respond: Boolean::False,
            });
            event.send_sync_service(
                runtime.syncer_state.monero_syncer(),
                SyncMsg::Task(abort_all.clone()),
            )?;
            event.send_sync_service(
                runtime.syncer_state.bitcoin_syncer(),
                SyncMsg::Task(abort_all),
            )?;
            runtime.txs.remove(&TxLabel::Cancel);
            runtime.txs.remove(&TxLabel::Punish);
            Ok(Some(SwapStateMachine::SwapEnd(Outcome::SuccessSwap)))
        }
        _ => Ok(None),
    }
}

fn try_alice_canceled_to_alice_refund_or_alice_punish(
    mut event: Event,
    runtime: &mut Runtime,
    alice_canceled: AliceCanceled,
) -> Result<Option<SwapStateMachine>, Error> {
    let AliceCanceled {
        remote_params,
        adaptor_refund,
        mut swap_key_manager,
    } = alice_canceled;
    match event.request.clone() {
        BusMsg::Sync(SyncMsg::Event(SyncEvent::TransactionConfirmations(
            TransactionConfirmations {
                id,
                confirmations: Some(confirmations),
                ref tx,
                ..
            },
        ))) => {
            match runtime.syncer_state.tasks.watched_txs.get(&id) {
                // Alice can punish once Cancel is final and the punish timelock is expired
                Some(&TxLabel::Cancel)
                    if runtime
                        .temporal_safety
                        .final_tx(confirmations, Blockchain::Bitcoin)
                        && runtime.temporal_safety.valid_punish(confirmations)
                        && runtime.txs.contains_key(&TxLabel::Punish) =>
                {
                    runtime.log_debug("Publishing punish tx");
                    let (tx_label, punish_tx) = runtime.txs.remove_entry(&TxLabel::Punish).unwrap();
                    // syncer's watch punish tx task
                    let txid = punish_tx.txid();
                    let task = runtime.syncer_state.watch_tx_btc(txid, tx_label);
                    event.send_sync_service(
                        runtime.syncer_state.bitcoin_syncer(),
                        SyncMsg::Task(task),
                    )?;
                    runtime.broadcast(punish_tx, tx_label, event.endpoints)?;
                    Ok(Some(SwapStateMachine::AliceCanceled(AliceCanceled {
                        remote_params,
                        adaptor_refund,
                        swap_key_manager,
                    })))
                }

                // When Alice's Punish transaction is final, end the swap
                Some(&TxLabel::Punish)
                    if runtime
                        .temporal_safety
                        .final_tx(confirmations, Blockchain::Bitcoin) =>
                {
                    let abort_all = Task::Abort(Abort {
                        task_target: TaskTarget::AllTasks,
                        respond: Boolean::False,
                    });
                    event.send_sync_service(
                        runtime.syncer_state.monero_syncer(),
                        SyncMsg::Task(abort_all.clone()),
                    )?;
                    event.send_sync_service(
                        runtime.syncer_state.bitcoin_syncer(),
                        SyncMsg::Task(abort_all),
                    )?;
                    // remove txs to invalidate outdated states
                    runtime.txs.remove(&TxLabel::Cancel);
                    runtime.txs.remove(&TxLabel::Refund);
                    runtime.txs.remove(&TxLabel::Buy);
                    runtime.txs.remove(&TxLabel::Punish);
                    let outcome = Outcome::FailurePunish;
                    Ok(Some(SwapStateMachine::SwapEnd(outcome)))
                }

                // Hit this path if Alice overfunded, moved on to AliceCanceled, but
                // could not broadcast cancel yet since not available, so broadcast
                // if available now. Note that this will also broadcast if Bob
                // broadcasted cancel, which is fine.
                Some(&TxLabel::Lock)
                    if runtime
                        .temporal_safety
                        .final_tx(confirmations, Blockchain::Bitcoin)
                        && runtime.temporal_safety.valid_cancel(confirmations)
                        && runtime.txs.contains_key(&TxLabel::Cancel) =>
                {
                    runtime.log_debug("Publishing cancel tx");
                    let (tx_label, cancel_tx) = runtime.txs.remove_entry(&TxLabel::Cancel).unwrap();
                    // syncer's watch cancel tx task
                    let txid = cancel_tx.txid();
                    let task = runtime.syncer_state.watch_tx_btc(txid, tx_label);
                    event.send_sync_service(
                        runtime.syncer_state.bitcoin_syncer(),
                        SyncMsg::Task(task),
                    )?;
                    runtime.broadcast(cancel_tx, tx_label, event.endpoints)?;
                    Ok(Some(SwapStateMachine::AliceCanceled(AliceCanceled {
                        remote_params,
                        adaptor_refund,
                        swap_key_manager,
                    })))
                }

                // When Alice learns of the refund transaction, immediately extract the Monero keys from its adaptor signature
                Some(&TxLabel::Refund)
                    if runtime
                        .syncer_state
                        .tasks
                        .txids
                        .contains_key(&TxLabel::Refund) =>
                {
                    runtime.log_debug("Subscribe Refund address task");
                    let tx = bitcoin::Transaction::deserialize(
                        &tx.iter().flatten().copied().collect::<Vec<u8>>(),
                    )?;

                    log_tx_seen(runtime.swap_id, &TxLabel::Refund, &tx.txid().into());
                    let sweep_xmr = swap_key_manager.process_refund_tx(
                        &mut event,
                        runtime,
                        tx,
                        remote_params,
                        adaptor_refund,
                    )?;

                    runtime
                        .syncer_state
                        .tasks
                        .txids
                        .remove_entry(&TxLabel::Refund);

                    // Check if we already registered the lock transaction, if so, initiate sweeping procedure
                    runtime.log_debug(format!("{:?}", runtime.syncer_state.confirmations));
                    if runtime
                        .syncer_state
                        .confirmations
                        .get(&TxLabel::AccLock)
                        .is_some()
                    {
                        let task = runtime.syncer_state.sweep_xmr(sweep_xmr.clone(), true);
                        let sweep_address = if let Task::SweepAddress(sweep_address) = task {
                            sweep_address
                        } else {
                            return Ok(None);
                        };
                        runtime.log_monero_maturity(sweep_xmr.destination_address);
                        runtime.log_warn(
                            "Peerd might crash, just ignore it, counterparty closed \
                                    connection but you don't need it anymore!",
                        );
                        Ok(Some(SwapStateMachine::AliceRefund(sweep_address)))
                    } else {
                        if runtime.syncer_state.awaiting_funding {
                            runtime.log_warn(
                            "FundingCompleted never emitted, emitting it now to clean up farcasterd",
                        );
                            runtime.syncer_state.awaiting_funding = false;
                            event.send_ctl_service(
                                ServiceId::Farcasterd,
                                CtlMsg::FundingCompleted(Blockchain::Monero),
                            )?;
                        }
                        let abort_all = Task::Abort(Abort {
                            task_target: TaskTarget::AllTasks,
                            respond: Boolean::False,
                        });
                        event.send_sync_service(
                            runtime.syncer_state.monero_syncer(),
                            SyncMsg::Task(abort_all.clone()),
                        )?;
                        event.send_sync_service(
                            runtime.syncer_state.bitcoin_syncer(),
                            SyncMsg::Task(abort_all),
                        )?;
                        // remove txs to invalidate outdated states
                        runtime.txs.remove(&TxLabel::Cancel);
                        runtime.txs.remove(&TxLabel::Refund);
                        runtime.txs.remove(&TxLabel::Buy);
                        runtime.txs.remove(&TxLabel::Punish);
                        Ok(Some(SwapStateMachine::SwapEnd(Outcome::FailureRefund)))
                    }
                }
                _ => Ok(None),
            }
        }
        _ => Ok(None),
    }
}

fn try_alice_refund_to_alice_refund_sweeping(
    mut event: Event,
    runtime: &mut Runtime,
    sweep_address: SweepAddress,
) -> Result<Option<SwapStateMachine>, Error> {
    match event.request {
        BusMsg::Sync(SyncMsg::Event(SyncEvent::TransactionConfirmations(
            TransactionConfirmations {
                confirmations: Some(confirmations),
                ..
            },
        ))) if confirmations >= runtime.temporal_safety.sweep_monero_thr => {
            runtime.log_info(format!(
                "Monero are spendable now (height {}), sweeping ephemeral swap_key_manager",
                runtime.syncer_state.monero_height.label(),
            ));
            event.send_sync_service(
                runtime.syncer_state.monero_syncer(),
                SyncMsg::Task(Task::SweepAddress(sweep_address)),
            )?;
            Ok(Some(SwapStateMachine::AliceRefundSweeping))
        }
        _ => Ok(None),
    }
}

fn try_alice_refund_sweeping_to_swap_end(
    mut event: Event,
    runtime: &mut Runtime,
) -> Result<Option<SwapStateMachine>, Error> {
    match event.request {
        BusMsg::Sync(SyncMsg::Event(SyncEvent::SweepSuccess(SweepSuccess { id, .. })))
            if runtime.syncer_state.tasks.sweeping_addr == Some(id) =>
        {
            if runtime.syncer_state.awaiting_funding {
                runtime.log_warn(
                    "FundingCompleted never emitted, but not possible to sweep \
                        monero without passing through funding completed: \
                        emitting it now to clean up farcasterd",
                );
                runtime.syncer_state.awaiting_funding = false;
                event.send_ctl_service(
                    ServiceId::Farcasterd,
                    CtlMsg::FundingCompleted(Blockchain::Monero),
                )?;
            }
            let abort_all = Task::Abort(Abort {
                task_target: TaskTarget::AllTasks,
                respond: Boolean::False,
            });
            event.send_sync_service(
                runtime.syncer_state.monero_syncer(),
                SyncMsg::Task(abort_all.clone()),
            )?;
            event.send_sync_service(
                runtime.syncer_state.bitcoin_syncer(),
                SyncMsg::Task(abort_all),
            )?;
            // remove txs to invalidate outdated states
            runtime.txs.remove(&TxLabel::Cancel);
            runtime.txs.remove(&TxLabel::Refund);
            runtime.txs.remove(&TxLabel::Buy);
            runtime.txs.remove(&TxLabel::Punish);
            Ok(Some(SwapStateMachine::SwapEnd(Outcome::FailureRefund)))
        }
        _ => Ok(None),
    }
}

fn try_bob_buy_sweeping_to_swap_end(
    mut event: Event,
    runtime: &mut Runtime,
) -> Result<Option<SwapStateMachine>, Error> {
    match event.request {
        BusMsg::Sync(SyncMsg::Event(SyncEvent::SweepSuccess(SweepSuccess { id, .. })))
            if runtime.syncer_state.tasks.sweeping_addr == Some(id) =>
        {
            if runtime.syncer_state.awaiting_funding {
                runtime.log_warn(
                    "FundingCompleted never emitted, emitting it now to clean up farcasterd stats",
                );
                runtime.syncer_state.awaiting_funding = false;
                event.send_ctl_service(
                    ServiceId::Farcasterd,
                    CtlMsg::FundingCompleted(Blockchain::Bitcoin),
                )?;
            }
            let abort_all = Task::Abort(Abort {
                task_target: TaskTarget::AllTasks,
                respond: Boolean::False,
            });
            event.send_sync_service(
                runtime.syncer_state.monero_syncer(),
                SyncMsg::Task(abort_all.clone()),
            )?;
            event.send_sync_service(
                runtime.syncer_state.bitcoin_syncer(),
                SyncMsg::Task(abort_all),
            )?;
            // remove txs to invalidate outdated states
            runtime.txs.remove(&TxLabel::Cancel);
            runtime.txs.remove(&TxLabel::Refund);
            runtime.txs.remove(&TxLabel::Buy);
            runtime.txs.remove(&TxLabel::Punish);
            Ok(Some(SwapStateMachine::SwapEnd(Outcome::SuccessSwap)))
        }
        _ => Ok(None),
    }
}

fn attempt_transition_to_bob_reveal(
    event: Event,
    runtime: &mut Runtime,
    remote_commit: CommitAliceParameters,
    mut swap_key_manager: BobSwapKeyManager,
) -> Result<Option<SwapStateMachine>, Error> {
    match event.request.clone() {
        BusMsg::P2p(PeerMsg::Reveal(Reveal::Alice { parameters, proof })) => {
            runtime.log_info("Handling reveal with swap_key_manager");
            let (bob_reveal, remote_params) =
                swap_key_manager.handle_alice_reveals(runtime, parameters, proof, remote_commit)?;

            // The swap_key_manager only returns reveal if we are Bob Maker
            if let Some(bob_reveal) = bob_reveal {
                runtime.send_peer(event.endpoints, PeerMsg::Reveal(bob_reveal))?;
            }

            // start watching bitcoin fee estimate
            runtime.syncer_state.watch_bitcoin_fee(event.endpoints)?;

            Ok(Some(SwapStateMachine::BobReveal(BobReveal {
                remote_params,
                swap_key_manager,
            })))
        }
        BusMsg::Ctl(CtlMsg::AbortSwap) => handle_bob_abort_swap(event, runtime, swap_key_manager),
        _ => Ok(None),
    }
}

fn attempt_transition_to_alice_reveal(
    event: Event,
    runtime: &mut Runtime,
    remote_commit: CommitBobParameters,
    mut swap_key_manager: AliceSwapKeyManager,
) -> Result<Option<SwapStateMachine>, Error> {
    match event.request {
        BusMsg::P2p(PeerMsg::Reveal(Reveal::Bob { parameters, proof })) => {
            runtime.log_info("Handling reveal with swap_key_manager");
            let (alice_reveal, remote_params) =
                swap_key_manager.handle_bob_reveals(runtime, parameters, proof, remote_commit)?;

            // The swap_key_manager only returns reveal if we are Alice Maker
            if let Some(alice_reveal) = alice_reveal {
                runtime.send_peer(event.endpoints, PeerMsg::Reveal(alice_reveal))?;
            }
            Ok(Some(SwapStateMachine::AliceReveal(AliceReveal {
                remote_params,
                swap_key_manager,
            })))
        }
        BusMsg::Ctl(CtlMsg::AbortSwap) => handle_abort_swap(event, runtime),
        _ => Ok(None),
    }
}

/// Checks whether Bob can cancel the swap and does so if possible.
/// Throws a warning if Bob tries to abort since swap already locked in.
fn handle_bob_swap_interrupt_after_lock(
    mut event: Event,
    runtime: &mut Runtime,
) -> Result<Option<SwapStateMachine>, Error> {
    match event.request {
        BusMsg::Ctl(CtlMsg::AbortSwap) => handle_abort_impossible(event, runtime),

        BusMsg::Sync(SyncMsg::Event(SyncEvent::TransactionConfirmations(
            TransactionConfirmations {
                id,
                confirmations: Some(confirmations),
                ..
            },
        ))) if runtime
            .temporal_safety
            .final_tx(confirmations, Blockchain::Bitcoin)
            && runtime.syncer_state.tasks.watched_txs.get(&id) == Some(&TxLabel::Lock)
            && runtime.temporal_safety.valid_cancel(confirmations)
            && runtime.txs.contains_key(&TxLabel::Cancel) =>
        {
            let watch_addr_task = runtime
                .bob_watch_cancel_output_task()
                .expect("just checked TxLabel::Cancel exists");
            event.send_sync_service(
                runtime.syncer_state.bitcoin_syncer(),
                SyncMsg::Task(watch_addr_task),
            )?;

            let (tx_label, cancel_tx) = runtime.txs.remove_entry(&TxLabel::Cancel).unwrap();
            runtime.broadcast(cancel_tx, tx_label, event.endpoints)?;
            Ok(None)
        }

        BusMsg::Sync(SyncMsg::Event(SyncEvent::TransactionConfirmations(
            TransactionConfirmations {
                id,
                confirmations: Some(_),
                ..
            },
        ))) if runtime.syncer_state.tasks.watched_txs.get(&id) == Some(&TxLabel::Cancel) => {
            if let Some(watch_addr_task) = runtime.bob_watch_cancel_output_task() {
                event.send_sync_service(
                    runtime.syncer_state.bitcoin_syncer(),
                    SyncMsg::Task(watch_addr_task),
                )?;
            };
            Ok(Some(SwapStateMachine::BobCanceled))
        }
        _ => Ok(None),
    }
}

/// Checks whether the swap has already been cancelled.
/// Checks whether Alice can cancel the swap and does so if possible.
/// Throws a warning if Alice tries to abort since swap already locked in.
fn handle_alice_swap_interrupt_after_lock(
    mut event: Event,
    runtime: &mut Runtime,
    remote_params: Parameters,
    adaptor_refund: WrappedEncryptedSignature,
    swap_key_manager: AliceSwapKeyManager,
) -> Result<Option<SwapStateMachine>, Error> {
    match event.request {
        BusMsg::Sync(SyncMsg::Event(SyncEvent::TransactionConfirmations(
            TransactionConfirmations {
                id,
                confirmations: Some(_),
                ..
            },
        ))) if runtime.syncer_state.tasks.watched_txs.get(&id) == Some(&TxLabel::Cancel) => {
            runtime.log_warn("This swap was canceled. Do not fund anymore.");
            if runtime.syncer_state.awaiting_funding {
                event.send_ctl_service(
                    ServiceId::Farcasterd,
                    CtlMsg::FundingCanceled(Blockchain::Monero),
                )?;
                runtime.syncer_state.awaiting_funding = false;
            }
            Ok(Some(SwapStateMachine::AliceCanceled(AliceCanceled {
                remote_params,
                adaptor_refund,
                swap_key_manager,
            })))
        }
        BusMsg::Sync(SyncMsg::Event(SyncEvent::TransactionConfirmations(
            TransactionConfirmations {
                id,
                confirmations: Some(confirmations),
                ..
            },
        ))) if runtime.syncer_state.tasks.watched_txs.get(&id) == Some(&TxLabel::Lock)
            && runtime.temporal_safety.valid_cancel(confirmations)
            && runtime.txs.contains_key(&TxLabel::Cancel) =>
        {
            let (tx_label, cancel_tx) = runtime.txs.remove_entry(&TxLabel::Cancel).unwrap();
            runtime.broadcast(cancel_tx, tx_label, event.endpoints)?;
            Ok(None)
        }
        BusMsg::Sync(SyncMsg::Event(SyncEvent::TransactionConfirmations(
            TransactionConfirmations {
                id,
                confirmations: Some(_),
                ..
            },
        ))) if runtime.syncer_state.tasks.watched_txs.get(&id) == Some(&TxLabel::Cancel) => {
            Ok(Some(SwapStateMachine::AliceCanceled(AliceCanceled {
                remote_params,
                adaptor_refund,
                swap_key_manager,
            })))
        }
        BusMsg::Ctl(CtlMsg::AbortSwap) => handle_abort_impossible(event, runtime),

        _ => Ok(None),
    }
}

fn handle_abort_swap(
    event: Event,
    runtime: &mut Runtime,
) -> Result<Option<SwapStateMachine>, Error> {
    event.complete_client_info(InfoMsg::String("Aborted swap".to_string()))?;
    runtime.log_info("Aborted swap.");
    Ok(Some(SwapStateMachine::SwapEnd(Outcome::FailureAbort)))
}

fn handle_abort_impossible(
    event: Event,
    runtime: &mut Runtime,
) -> Result<Option<SwapStateMachine>, Error> {
    let msg = "Swap is already locked-in, cannot manually abort anymore.".to_string();
    runtime.log_warn(&msg);
    event.complete_client_ctl(CtlMsg::Failure(Failure {
        code: FailureCode::Unknown,
        info: msg,
    }))?;
    Ok(None)
}

fn handle_bob_abort_swap(
    mut event: Event,
    runtime: &mut Runtime,
    mut swap_key_manager: BobSwapKeyManager,
) -> Result<Option<SwapStateMachine>, Error> {
    let funding_address = swap_key_manager
        .funding_address()
        .expect("Am Bob, so have funding address");
    let sweep_btc = swap_key_manager.process_get_sweep_bitcoin_address(funding_address)?;
    runtime.log_info(format!(
        "Sweeping source (funding) address: {} to destination address: {}",
        sweep_btc.source_address.addr(),
        sweep_btc.destination_address.addr()
    ));
    let task = runtime.syncer_state.sweep_btc(sweep_btc, false);
    event.send_sync_service(runtime.syncer_state.bitcoin_syncer(), SyncMsg::Task(task))?;
    event.complete_client_info(InfoMsg::String(
        "Aborting swap, checking if funds can be sweeped.".to_string(),
    ))?;
    Ok(Some(SwapStateMachine::BobAbortAwaitingBitcoinSweep))
}
