// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use farcaster_core::blockchain::{Blockchain, Network};

use crate::{
    bus::ctl::CtlMsg,
    bus::info::InfoMsg,
    bus::BusMsg,
    bus::{sync::SyncMsg, AddressSecretKey, Failure, FailureCode},
    error::Error,
    event::{Event, StateMachine, StateMachineExecutor},
    syncerd::{
        Event as SyncerEvent, GetAddressBalance, Health, HealthCheck, SweepAddress,
        SweepAddressAddendum, Task, TaskAborted, TaskId,
    },
    ServiceId,
};

use super::runtime::{syncer_up, Runtime};

/// State machine for making a syncer request from and to a client.
/// State machine automaton:
/// ```ignore
///        Start
///          |
///    ______|_______
///   |             |
///   |             V
///   |      AwaitingSyncer
///   |             |
///   V             V
/// AwaitingSyncerRequest
///          |
///          V
///         End
/// ```
#[derive(Display)]
pub enum SyncerStateMachine {
    /// Start state - transitions to AwaitingSyncer or AwaitingSyncerRequest on
    /// cli request or None on failure.  Transition to AwaitingSyncer triggers
    /// launch syncer.  Transition to AwaitingSyncerRequest triggers a request
    /// to the target syncer.
    #[display("Start")]
    Start,

    /// AwaitingSyncer state - transitions to AwaitingSyncerRequest once the
    /// syncer Hello is reiceved. Transition to AwaitingSyncerRequest triggers a
    /// request to the target syncer.
    #[display("Awaiting Syncer")]
    AwaitingSyncer(AwaitingSyncer),

    /// AwaitingSyncerRequest state - transitions to None on success. Transition
    /// to None triggers a response back to the client.
    #[display("Awaiting Syncer Request")]
    AwaitingSyncerRequest(AwaitingSyncerRequest),
}

pub struct AwaitingSyncer {
    source: ServiceId,
    syncer: ServiceId,
    syncer_task: Task,
    syncer_task_id: TaskId,
}

pub struct AwaitingSyncerRequest {
    source: ServiceId,
    syncer: ServiceId,
    syncer_task_id: TaskId,
}

impl StateMachine<Runtime, Error> for SyncerStateMachine {
    fn next(self, event: Event, runtime: &mut Runtime) -> Result<Option<Self>, Error> {
        match self {
            SyncerStateMachine::Start => {
                attempt_transition_to_awaiting_syncer_or_awaiting_syncer_request(event, runtime)
            }
            SyncerStateMachine::AwaitingSyncer(awaiting_syncer) => {
                attempt_transition_to_awaiting_syncer_request(event, runtime, awaiting_syncer)
            }
            SyncerStateMachine::AwaitingSyncerRequest(awaiting_syncer_request) => {
                attempt_transition_to_end(event, runtime, awaiting_syncer_request)
            }
        }
    }

    fn name(&self) -> String {
        "Syncer".to_string()
    }
}

pub struct SyncerStateMachineExecutor {}
impl StateMachineExecutor<Runtime, Error, SyncerStateMachine> for SyncerStateMachineExecutor {}

impl SyncerStateMachine {
    pub fn task_id(&self) -> Option<TaskId> {
        match self {
            SyncerStateMachine::AwaitingSyncer(AwaitingSyncer { syncer_task_id, .. }) => {
                Some(*syncer_task_id)
            }
            SyncerStateMachine::AwaitingSyncerRequest(AwaitingSyncerRequest {
                syncer_task_id,
                ..
            }) => Some(*syncer_task_id),
            _ => None,
        }
    }

    pub fn syncer(&self) -> Option<ServiceId> {
        match self {
            SyncerStateMachine::AwaitingSyncer(AwaitingSyncer { syncer, .. }) => {
                Some(syncer.clone())
            }
            SyncerStateMachine::AwaitingSyncerRequest(AwaitingSyncerRequest { syncer, .. }) => {
                Some(syncer.clone())
            }
            _ => None,
        }
    }
}

fn attempt_transition_to_awaiting_syncer_or_awaiting_syncer_request(
    event: Event,
    runtime: &mut Runtime,
) -> Result<Option<SyncerStateMachine>, Error> {
    let source = event.source.clone();
    match event.request.clone() {
        BusMsg::Ctl(CtlMsg::SweepAddress(sweep_address)) => {
            let (blockchain, network) = match sweep_address.clone() {
                SweepAddressAddendum::Monero(addendum) => {
                    let blockchain = Blockchain::Monero;
                    let mut network = addendum.destination_address.network.into();

                    // Switch the network to local if the mainnet configuration does
                    // not exist and the local network exists
                    network = if network == Network::Mainnet
                        && runtime.config.get_syncer_servers(network).is_none()
                        && runtime.config.get_syncer_servers(Network::Local).is_some()
                    {
                        Network::Local
                    } else {
                        network
                    };
                    (blockchain, network)
                }
                SweepAddressAddendum::Bitcoin(addendum) => {
                    (Blockchain::Bitcoin, addendum.source_address.network.into())
                }
            };

            let syncer_task_id = TaskId(runtime.syncer_task_counter);
            let syncer_task = Task::SweepAddress(SweepAddress {
                id: syncer_task_id,
                retry: false,
                lifetime: u64::MAX,
                addendum: sweep_address,
            });
            runtime.syncer_task_counter += 1;

            // check if a monero syncer is up
            if let Some(service_id) = syncer_up(
                &mut runtime.spawning_services,
                &mut runtime.registered_services,
                blockchain,
                network,
                &runtime.config,
            )? {
                event.complete_sync_service(service_id, SyncMsg::Task(syncer_task))?;
                Ok(Some(SyncerStateMachine::AwaitingSyncerRequest(
                    AwaitingSyncerRequest {
                        source,
                        syncer_task_id,
                        syncer: ServiceId::Syncer(blockchain, network),
                    },
                )))
            } else {
                Ok(Some(SyncerStateMachine::AwaitingSyncer(AwaitingSyncer {
                    source,
                    syncer: ServiceId::Syncer(blockchain, network),
                    syncer_task,
                    syncer_task_id,
                })))
            }
        }

        BusMsg::Ctl(CtlMsg::GetBalance(address_secret_key)) => {
            let syncer_task_id = TaskId(runtime.syncer_task_counter);
            runtime.syncer_task_counter += 1;
            let (blockchain, network) = match &address_secret_key {
                AddressSecretKey::Bitcoin { address, .. } => {
                    (Blockchain::Bitcoin, address.network.into())
                }
                AddressSecretKey::Monero { address, .. } => {
                    (Blockchain::Monero, address.network.into())
                }
            };
            let syncer_task = Task::GetAddressBalance(GetAddressBalance {
                id: syncer_task_id,
                address_secret_key,
            });
            // check if a monero syncer is up
            if let Some(service_id) = syncer_up(
                &mut runtime.spawning_services,
                &mut runtime.registered_services,
                blockchain,
                network,
                &runtime.config,
            )? {
                event.complete_sync_service(service_id, SyncMsg::Task(syncer_task))?;
                Ok(Some(SyncerStateMachine::AwaitingSyncerRequest(
                    AwaitingSyncerRequest {
                        source,
                        syncer_task_id,
                        syncer: ServiceId::Syncer(blockchain, network),
                    },
                )))
            } else {
                Ok(Some(SyncerStateMachine::AwaitingSyncer(AwaitingSyncer {
                    source,
                    syncer: ServiceId::Syncer(blockchain, network),
                    syncer_task,
                    syncer_task_id,
                })))
            }
        }

        BusMsg::Ctl(CtlMsg::HealthCheck(blockchain, network)) => {
            let syncer_task_id = TaskId(runtime.syncer_task_counter);
            runtime.syncer_task_counter += 1;
            let syncer_task = Task::HealthCheck(HealthCheck { id: syncer_task_id });

            match syncer_up(
                &mut runtime.spawning_services,
                &mut runtime.registered_services,
                blockchain,
                network,
                &runtime.config,
            ) {
                Ok(Some(service_id)) => {
                    event.complete_sync_service(service_id, SyncMsg::Task(syncer_task))?;
                    Ok(Some(SyncerStateMachine::AwaitingSyncerRequest(
                        AwaitingSyncerRequest {
                            source,
                            syncer_task_id,
                            syncer: ServiceId::Syncer(blockchain, network),
                        },
                    )))
                }
                Ok(None) => Ok(Some(SyncerStateMachine::AwaitingSyncer(AwaitingSyncer {
                    source,
                    syncer: ServiceId::Syncer(blockchain, network),
                    syncer_task,
                    syncer_task_id,
                }))),
                Err(err) => {
                    event.complete_ctl(CtlMsg::HealthResult(Health::ConfigUnavailable(
                        err.to_string(),
                    )))?;
                    Ok(None)
                }
            }
        }

        req => {
            warn!(
                "Request {} from {} invalid for state start - invalidating.",
                req, event.source
            );
            Ok(None)
        }
    }
}

fn attempt_transition_to_awaiting_syncer_request(
    event: Event,
    _runtime: &mut Runtime,
    awaiting_syncer: AwaitingSyncer,
) -> Result<Option<SyncerStateMachine>, Error> {
    let AwaitingSyncer {
        source,
        syncer,
        syncer_task,
        syncer_task_id,
    } = awaiting_syncer;
    match (event.request.clone(), event.source.clone()) {
        (BusMsg::Ctl(CtlMsg::Hello), syncer_id) if syncer == syncer_id => {
            event.complete_sync_service(syncer.clone(), SyncMsg::Task(syncer_task))?;
            Ok(Some(SyncerStateMachine::AwaitingSyncerRequest(
                AwaitingSyncerRequest {
                    source,
                    syncer_task_id,
                    syncer,
                },
            )))
        }
        (req, source) => {
            if let BusMsg::Ctl(CtlMsg::Hello) = req {
                trace!(
                    "BusMsg {} from {} invalid for state awaiting syncer.",
                    req,
                    source
                );
            } else {
                warn!(
                    "BusMsg {} from {} invalid for state awaiting syncer.",
                    req, source
                );
            }
            Ok(Some(SyncerStateMachine::AwaitingSyncer(AwaitingSyncer {
                source,
                syncer,
                syncer_task,
                syncer_task_id,
            })))
        }
    }
}

fn attempt_transition_to_end(
    mut event: Event,
    runtime: &mut Runtime,
    awaiting_syncer_request: AwaitingSyncerRequest,
) -> Result<Option<SyncerStateMachine>, Error> {
    let AwaitingSyncerRequest {
        syncer_task_id,
        source,
        syncer,
    } = awaiting_syncer_request;
    match (event.request.clone(), event.source.clone()) {
        (BusMsg::Sync(SyncMsg::Event(SyncerEvent::SweepSuccess(mut success))), syncer_id)
            if syncer == syncer_id && success.id == syncer_task_id =>
        {
            if let Some(txid) = success.txids.pop() {
                event.send_client_info(
                    source,
                    InfoMsg::String(format!(
                        "Successfully sweeped address. Transaction Id: {}.",
                        txid
                    )),
                )?;
            } else {
                event.send_client_info(source, InfoMsg::String("Nothing to sweep.".to_string()))?;
            }
            runtime.clean_up_after_syncer_usage(event.endpoints)?;
            Ok(None)
        }

        (BusMsg::Sync(SyncMsg::Event(SyncerEvent::AddressBalance(res))), syncer_id)
            if syncer == syncer_id && res.id == syncer_task_id =>
        {
            if let Some(err) = res.err {
                event.send_client_ctl(
                    source,
                    CtlMsg::Failure(Failure {
                        code: FailureCode::Unknown,
                        info: format!("Failed to get adddress balance {}", err),
                    }),
                )?;
            } else {
                event.send_client_info(
                    source,
                    InfoMsg::AddressBalance(crate::bus::info::AddressBalance {
                        address: res.address,
                        balance: res.balance,
                    }),
                )?;
            }
            runtime.clean_up_after_syncer_usage(event.endpoints)?;
            Ok(None)
        }

        (
            BusMsg::Sync(SyncMsg::Event(SyncerEvent::TaskAborted(TaskAborted {
                id, error, ..
            }))),
            syncer_id,
        ) if syncer == syncer_id && id.len() == 1 && id[0] == syncer_task_id => {
            event.send_client_ctl(
                source,
                CtlMsg::Failure(Failure {
                    code: FailureCode::Unknown,
                    info: format!(
                        "Failure in chain query{}",
                        error.map_or("".to_string(), |e| format!(": {}", e))
                    ),
                }),
            )?;
            runtime.clean_up_after_syncer_usage(event.endpoints)?;
            Ok(None)
        }

        (BusMsg::Sync(SyncMsg::Event(SyncerEvent::HealthResult(res))), syncer_id)
            if syncer == syncer_id && res.id == syncer_task_id =>
        {
            event.send_client_ctl(source, CtlMsg::HealthResult(res.health))?;
            runtime.clean_up_after_syncer_usage(event.endpoints)?;
            Ok(None)
        }
        (req, source) => {
            if let BusMsg::Ctl(CtlMsg::Hello) = req {
                trace!(
                    "BusMsg {} from {} invalid for state awaiting syncer.",
                    req,
                    source
                );
            } else {
                warn!(
                    "BusMsg {} from {} invalid for state awaiting syncer.",
                    req, source
                );
            }
            Ok(Some(SyncerStateMachine::AwaitingSyncerRequest(
                AwaitingSyncerRequest {
                    syncer_task_id,
                    source,
                    syncer,
                },
            )))
        }
    }
}
