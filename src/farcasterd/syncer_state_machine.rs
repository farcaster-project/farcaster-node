use bitcoin::hashes::{hex::ToHex, Hash};
use farcaster_core::blockchain::{Blockchain, Network};

use crate::{
    error::Error,
    event::{Event, StateMachine},
    rpc::Request,
    syncerd::{Event as SyncerEvent, SweepAddress, SweepAddressAddendum, Task, TaskId},
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
}

impl SyncerStateMachine {
    pub fn task_id(&self) -> Option<TaskId> {
        match self {
            SyncerStateMachine::AwaitingSyncer(AwaitingSyncer { syncer_task_id, .. }) => {
                Some(syncer_task_id.clone())
            }
            SyncerStateMachine::AwaitingSyncerRequest(AwaitingSyncerRequest {
                syncer_task_id,
                ..
            }) => Some(syncer_task_id.clone()),
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
        Request::SweepAddress(sweep_address) => {
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
                id: syncer_task_id.clone(),
                retry: false,
                lifetime: u64::MAX,
                addendum: sweep_address,
                from_height: None,
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
                event.complete_ctl_service(service_id, Request::SyncerTask(syncer_task))?;
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
                    syncer_task: syncer_task,
                    syncer_task_id,
                })))
            }
        }

        _ => Ok(None),
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
        (Request::Hello, syncer_id) if syncer == syncer_id => {
            event.complete_ctl_service(syncer.clone(), Request::SyncerTask(syncer_task))?;
            Ok(Some(SyncerStateMachine::AwaitingSyncerRequest(
                AwaitingSyncerRequest {
                    source,
                    syncer_task_id,
                    syncer,
                },
            )))
        }
        (req, source) => {
            if let Request::Hello = req {
                trace!(
                    "Request {} from {} invalid for state awaiting syncer.",
                    req,
                    source
                );
            } else {
                warn!(
                    "Request {} from {} invalid for state awaiting syncer.",
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
        (Request::SyncerEvent(SyncerEvent::SweepSuccess(success)), syncer_id)
            if syncer == syncer_id && success.id == syncer_task_id =>
        {
            if let Some(Some(txid)) = success
                .txids
                .clone()
                .pop()
                .map(|txid| bitcoin::Txid::from_slice(&txid).ok())
            {
                event.send_ctl_service(
                    source,
                    Request::String(format!(
                        "Successfully sweeped address. Transaction Id: {}.",
                        txid.to_hex()
                    )),
                )?;
            } else {
                event.send_ctl_service(source, Request::String("Nothing to sweep.".to_string()))?;
            }

            runtime.registered_services = runtime
                .registered_services
                .clone()
                .drain()
                .filter(|service| {
                    if let ServiceId::Syncer(..) = service {
                        if !runtime.syncer_has_client(service) {
                            info!("Terminating {}", service);
                            event
                                .send_ctl_service(service.clone(), Request::Terminate)
                                .is_err()
                        } else {
                            true
                        }
                    } else {
                        true
                    }
                })
                .collect();
            Ok(None)
        }
        (req, source) => {
            if let Request::Hello = req {
                trace!(
                    "Request {} from {} invalid for state awaiting syncer.",
                    req,
                    source
                );
            } else {
                warn!(
                    "Request {} from {} invalid for state awaiting syncer.",
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
