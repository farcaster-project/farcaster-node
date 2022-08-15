use crate::farcaster_core::{
    blockchain::{Blockchain, Network},
    consensus::Decodable,
};
use crate::rpc::request::SyncerdBridgeEvent;
use crate::syncerd::runtime::SyncerdTask;
use crate::syncerd::{TaskId, TaskTarget};
use crate::Error;
use crate::ServiceId;
use microservices::rpc::Request;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::io;
use std::marker::{Send, Sized};
use tokio::sync::mpsc::Sender as TokioSender;

use crate::serde::Deserialize;
use crate::service::LogStyle;
use crate::syncerd::*;
use hex;

#[derive(Debug, Clone, Copy, Eq, PartialEq, PartialOrd, Hash, Display)]
#[display(Debug)]
pub struct InternalId(u32);

impl From<TaskCounter> for InternalId {
    fn from(counter: TaskCounter) -> Self {
        Self(counter.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Display, Debug)]
#[display(Debug)]
struct TaskCounter(pub u32);

impl TaskCounter {
    fn increment(&mut self) {
        self.0 += 1;
    }
}

pub struct SyncerState {
    blockchain: Blockchain,
    block_height: u64,
    block_hash: Vec<u8>,
    tasks_sources: HashMap<InternalId, ServiceId>,
    watch_height: HashMap<InternalId, WatchHeight>,
    watch_fee_estimation: HashMap<InternalId, WatchEstimateFee>,
    lifetimes: HashMap<u64, HashSet<InternalId>>,
    pub addresses: HashMap<InternalId, AddressTransactions>,
    pub transactions: HashMap<InternalId, WatchedTransaction>,
    pub unseen_transactions: HashSet<InternalId>,
    pub sweep_addresses: HashMap<InternalId, SweepAddress>,
    tx_event: TokioSender<SyncerdBridgeEvent>,
    task_count: TaskCounter,
    pub subscribed_addresses: HashSet<AddressAddendum>,
    pub fee_estimation: Option<FeeEstimations>,
}

#[derive(Clone, Debug)]
pub struct WatchedTransaction {
    pub task: WatchTransaction,
    pub transaction_confirmations: TransactionConfirmations,
}

#[derive(Clone, Eq, PartialEq, Debug)]
pub struct AddressTransactions {
    pub task: WatchAddress,
    known_txs: HashSet<AddressTx>,
    pub subscribed: bool,
}

#[derive(Debug, Clone, PartialEq, Hash, Eq)]
pub struct AddressTx {
    pub our_amount: u64,
    pub tx_id: Vec<u8>,
    pub tx: Vec<u8>,
}

pub fn create_set<T: std::hash::Hash + Eq>(xs: Vec<T>) -> HashSet<T> {
    xs.into_iter().collect()
}

impl SyncerState {
    pub fn new(tx_event: TokioSender<SyncerdBridgeEvent>, blockchain: Blockchain) -> Self {
        Self {
            block_height: 0,
            block_hash: vec![0],
            tasks_sources: HashMap::new(),
            watch_height: HashMap::new(),
            watch_fee_estimation: HashMap::new(),
            lifetimes: HashMap::new(),
            addresses: HashMap::new(),
            transactions: HashMap::new(),
            unseen_transactions: HashSet::new(),
            sweep_addresses: HashMap::new(),
            tx_event,
            task_count: TaskCounter(0),
            blockchain,
            subscribed_addresses: HashSet::new(),
            fee_estimation: None,
        }
    }

    pub fn block_height(&self) -> u64 {
        self.block_height
    }

    pub async fn abort(
        &mut self,
        task_task_id_or_all_tasks: TaskTarget,
        source: ServiceId,
        respond: bool,
    ) {
        let task_id = match task_task_id_or_all_tasks {
            TaskTarget::AllTasks => None,
            TaskTarget::TaskId(task_id) => Some(task_id),
        };

        let mut aborted_ids: Vec<TaskId> = vec![];
        // check addresses tasks
        let ids: Vec<(InternalId, TaskId)> = self
            .addresses
            .iter()
            .filter_map(|(id, address_transaction)| {
                if task_id.is_none() || address_transaction.task.id == task_id.unwrap() {
                    Some((*id, address_transaction.task.id))
                } else {
                    None
                }
            })
            .collect();

        aborted_ids.append(
            &mut ids
                .iter()
                .filter_map(|(internal_id, found_task_id)| {
                    if let Some(source_id) = self.tasks_sources.get(internal_id) {
                        if *source_id == source {
                            self.remove_address(internal_id);
                            return Some(*found_task_id);
                        }
                    }
                    None
                })
                .collect(),
        );

        // check transactions tasks
        let ids: Vec<(InternalId, TaskId)> = self
            .transactions
            .iter()
            .filter_map(|(id, watched_transaction)| {
                if task_id.is_none() || watched_transaction.task.id == task_id.unwrap() {
                    Some((*id, watched_transaction.task.id))
                } else {
                    None
                }
            })
            .collect();
        aborted_ids.append(
            &mut ids
                .iter()
                .filter_map(|(internal_id, found_task_id)| {
                    if let Some(source_id) = self.tasks_sources.get(internal_id) {
                        if *source_id == source {
                            self.remove_transaction(internal_id);
                            return Some(*found_task_id);
                        }
                    }
                    None
                })
                .collect(),
        );

        // check height tasks
        let ids: Vec<(InternalId, TaskId)> = self
            .watch_height
            .iter()
            .filter_map(|(id, watch_height)| {
                if task_id.is_none() || watch_height.id == task_id.unwrap() {
                    Some((*id, watch_height.id))
                } else {
                    None
                }
            })
            .collect();
        aborted_ids.append(
            &mut ids
                .iter()
                .filter_map(|(internal_id, found_task_id)| {
                    if let Some(source_id) = self.tasks_sources.get(internal_id) {
                        if *source_id == source {
                            self.remove_height(internal_id);
                            return Some(*found_task_id);
                        }
                    }
                    None
                })
                .collect(),
        );

        // check sweep address tasks
        let ids: Vec<(InternalId, TaskId)> = self
            .sweep_addresses
            .iter()
            .filter_map(|(id, sweep_address)| {
                if task_id.is_none() || sweep_address.id == task_id.unwrap() {
                    Some((*id, sweep_address.id))
                } else {
                    None
                }
            })
            .collect();
        aborted_ids.append(
            &mut ids
                .iter()
                .filter_map(|(internal_id, found_task_id)| {
                    if let Some(source_id) = self.tasks_sources.get(internal_id) {
                        if *source_id == source {
                            self.remove_sweep_address(internal_id);
                            return Some(*found_task_id);
                        }
                    }
                    None
                })
                .collect(),
        );

        if aborted_ids.is_empty() {
            send_event(
                &self.tx_event,
                &mut vec![(
                    Event::TaskAborted(TaskAborted {
                        id: vec![],
                        error: Some(format!(
                            "abort failed, task from source {} not found",
                            source
                        )),
                    }),
                    source.clone(),
                )],
            )
            .await;
            return;
        }

        if respond {
            send_event(
                &self.tx_event,
                &mut vec![(
                    Event::TaskAborted(TaskAborted {
                        id: aborted_ids,
                        error: None,
                    }),
                    source.clone(),
                )],
            )
            .await;
        }
    }

    pub async fn watch_height(&mut self, task: WatchHeight, source: ServiceId) {
        // increment the count to use it as a unique internal id
        self.task_count.increment();
        // This is technically valid behavior; immediately prune the task for being past
        // its lifetime by never inserting it
        if let Err(e) = self.add_lifetime(task.lifetime, self.task_count.into()) {
            error!("{}", e);
            return;
        }
        self.watch_height
            .insert(self.task_count.into(), task.clone());
        self.tasks_sources
            .insert(self.task_count.into(), source.clone());

        if self.block_height != 0 {
            send_event(
                &self.tx_event,
                &mut vec![(
                    Event::HeightChanged(HeightChanged {
                        id: task.id,
                        block: self.block_hash.clone(),
                        height: self.block_height,
                    }),
                    source,
                )],
            )
            .await;
        }
    }

    pub fn watch_address(&mut self, task: WatchAddress, source: ServiceId) {
        // increment the count to use it as a unique internal id
        self.task_count.increment();
        if let Err(e) = self.add_lifetime(task.lifetime, self.task_count.into()) {
            error!("{}", e);
            return;
        };
        self.tasks_sources.insert(self.task_count.into(), source);
        let address_txs = AddressTransactions {
            task,
            known_txs: none!(),
            subscribed: false,
        };
        self.addresses.insert(self.task_count.into(), address_txs);
    }

    pub fn address_subscribed(&mut self, id: InternalId) {
        let address = self.addresses.get_mut(&id);
        if let Some(address) = address {
            self.subscribed_addresses
                .insert(address.task.addendum.clone());
        }
    }

    pub fn watch_transaction(&mut self, task: WatchTransaction, source: ServiceId) {
        // increment the count to use it as a unique internal id
        self.task_count.increment();

        if let Err(e) = self.add_lifetime(task.lifetime, self.task_count.into()) {
            error!("{}", e);
            return;
        };
        self.tasks_sources.insert(self.task_count.into(), source);
        self.transactions.insert(
            self.task_count.into(),
            WatchedTransaction {
                task: task.clone(),
                transaction_confirmations: TransactionConfirmations {
                    id: task.id,
                    block: none!(),
                    confirmations: None,
                    tx: vec![],
                },
            },
        );
        self.unseen_transactions.insert(self.task_count.into());
    }

    pub async fn estimate_fee(&mut self, task: WatchEstimateFee, source: ServiceId) {
        // increment the count to use it as a unique internal id
        self.task_count.increment();
        self.watch_fee_estimation
            .insert(self.task_count.into(), task.clone());
        self.tasks_sources
            .insert(self.task_count.into(), source.clone());

        // try to emit an event immediately from the cached values
        if let Some(ref fee_estimations) = &self.fee_estimation {
            send_event(
                &self.tx_event,
                &mut vec![(
                    Event::FeeEstimation(FeeEstimation {
                        id: task.id,
                        fee_estimations: fee_estimations.clone(),
                    }),
                    source,
                )],
            )
            .await;
        }
    }

    pub fn sweep_address(&mut self, task: SweepAddress, source: ServiceId) {
        self.task_count.increment();
        if let Some(lifetimes) = self.lifetimes.get_mut(&task.lifetime) {
            lifetimes.insert(self.task_count.into());
        } else {
            let mut lifetimes = HashSet::new();
            lifetimes.insert(self.task_count.into());
            self.lifetimes.insert(task.lifetime, lifetimes);
        }
        self.sweep_addresses.insert(self.task_count.into(), task);
        self.tasks_sources.insert(self.task_count.into(), source);
    }
    pub async fn change_height(&mut self, new_height: u64, block: Vec<u8>) -> bool {
        if self.block_height != new_height || self.block_hash != block {
            self.handle_change_height(new_height, block.clone());
            self.drop_lifetimes();

            // Emit a height_changed event
            for (id, task) in self.watch_height.iter() {
                send_event(
                    &self.tx_event,
                    &mut vec![(
                        Event::HeightChanged(HeightChanged {
                            id: task.id,
                            block: block.clone(),
                            height: self.block_height,
                        }),
                        self.tasks_sources.get(id).unwrap().clone(),
                    )],
                )
                .await;
            }
            true
        } else {
            false
        }
    }

    fn handle_change_height(&mut self, new_height: u64, block: Vec<u8>) {
        match (new_height, &block) {
            (h, b) if h > self.block_height && b != &self.block_hash => {
                self.block_height = h;
                self.block_hash = block;
                info!(
                    "{} incremented height {}",
                    self.blockchain.bright_white_bold(),
                    &h.bright_blue_bold()
                );
            }
            (h, b) if h == self.block_height && b != &self.block_hash => {
                self.block_hash = block;
                info!("{} new chain tip", self.blockchain.bright_white_bold());
            }
            (h, b) if h < self.block_height && b != &self.block_hash => {
                self.block_height = h;
                self.block_hash = block;
                warn!(
                    "{} height decreased {}, new chain tip",
                    self.blockchain.bright_white_bold(),
                    &h.bright_blue_bold()
                );
            }
            (_, b) if b == &self.block_hash => {
                error!("Unreachable: Block hash didnt change, ignoring")
            }
            _ => error!("Unexpected block height change, ignoring"),
        }
    }

    pub async fn change_address(
        &mut self,
        address_addendum: AddressAddendum,
        txs: HashSet<AddressTx>,
    ) {
        self.drop_lifetimes();
        let mut events: Vec<(Event, ServiceId)> = Vec::new();
        inner(
            &mut self.addresses,
            &mut events,
            &mut self.tasks_sources,
            address_addendum,
            txs,
        );

        fn inner(
            addresses: &mut HashMap<InternalId, AddressTransactions>,
            events: &mut Vec<(Event, ServiceId)>,
            tasks_sources: &mut HashMap<InternalId, ServiceId>,
            address_addendum: AddressAddendum,
            txs: HashSet<AddressTx>,
        ) {
            let changes_detected: bool = addresses
                .iter()
                .find_map(|(_, addr)| {
                    // let new_txs = txs.difference(&addr.known_txs).collect::<Vec<&_>>();
                    if txs.difference(&addr.known_txs).next().is_some() {
                        Some(true)
                    } else {
                        None
                    }
                })
                .unwrap_or(false);

            if !changes_detected {
                trace!("no changes to process, skipping...");
                return;
            }

            *addresses = addresses
                .drain()
                .map(|(id, addr)| {
                    trace!("processing taskid {} for address {:?}", id, addr);
                    if addr.task.addendum != address_addendum {
                        trace!("address not changed or not address_addendum of interest");
                        return (id, addr);
                    }
                    // create events for new transactions
                    for new_tx in txs.difference(&addr.known_txs).cloned() {
                        debug!("new tx seen: {}", hex::encode(&new_tx.tx_id));
                        let address_transaction = AddressTransaction {
                            id: addr.task.id,
                            hash: new_tx.tx_id,
                            amount: new_tx.our_amount,
                            block: vec![], // eventually this should be removed from the event
                            tx: new_tx.tx,
                        };
                        events.push((
                            Event::AddressTransaction(address_transaction),
                            tasks_sources
                                .get(&id)
                                .cloned()
                                .expect("task source missing"),
                        ));
                    }

                    (
                        id,
                        AddressTransactions {
                            task: addr.task,
                            known_txs: txs.clone(),
                            subscribed: addr.subscribed,
                        },
                    )
                })
                .collect();

            events.dedup();
        }

        send_event(&self.tx_event, &mut events).await;
    }

    pub async fn change_transaction(
        &mut self,
        tx_id: Vec<u8>,
        block_hash: Option<Vec<u8>>,
        confirmations: Option<u32>,
        tx: Vec<u8>,
    ) {
        self.drop_lifetimes();
        let mut events: Vec<(Event, ServiceId)> = Vec::new();
        inner(
            &mut self.transactions,
            &mut self.unseen_transactions,
            &mut events,
            &mut self.tasks_sources,
            tx_id,
            block_hash,
            confirmations,
            tx,
        );

        #[allow(clippy::too_many_arguments)]
        fn inner(
            transactions: &mut HashMap<InternalId, WatchedTransaction>,
            unseen_transactions: &mut HashSet<InternalId>,
            events: &mut Vec<(Event, ServiceId)>,
            tasks_sources: &mut HashMap<InternalId, ServiceId>,
            tx_id: Vec<u8>,
            block_hash: Option<Vec<u8>>,
            confirmations: Option<u32>,
            tx: Vec<u8>,
        ) {
            let block = match block_hash {
                Some(bh) => bh,
                // per RFC, no block hash should be encoded as 0x0
                None => hex::decode("00").unwrap(),
            };
            *transactions = transactions
                .iter()
                .filter_map(|(id, watched_tx)| {
                    // return unchanged transactions
                    if tx_id != watched_tx.task.hash {
                        return Some((*id, watched_tx.clone()));
                    }
                    if confirmations.is_some() {
                        unseen_transactions.remove(id);
                    } else {
                        unseen_transactions.insert(*id);
                    }
                    let transaction_confirmations = if confirmations
                        != watched_tx.transaction_confirmations.confirmations
                        || block != watched_tx.transaction_confirmations.block
                    {
                        let tx_confs = TransactionConfirmations {
                            id: watched_tx.task.id,
                            block: block.clone(),
                            confirmations,
                            tx: tx.clone(),
                        };
                        events.push((
                            Event::TransactionConfirmations(tx_confs.clone()),
                            tasks_sources.get(id).unwrap().clone(),
                        ));
                        tx_confs
                    } else {
                        watched_tx.transaction_confirmations.clone()
                    };
                    // prune the task once it has reached its confirmation bound
                    if confirmations >= Some(watched_tx.task.confirmation_bound) {
                        None
                    } else {
                        Some((
                            *id,
                            WatchedTransaction {
                                task: watched_tx.task.clone(),
                                transaction_confirmations,
                            },
                        ))
                    }
                })
                .collect();
        }

        send_event(&self.tx_event, &mut events).await;
    }

    pub async fn success_sweep(&mut self, id: &InternalId, txids: Vec<Vec<u8>>) {
        if let Some(sweep_address) = self.sweep_addresses.get(id) {
            send_event(
                &self.tx_event,
                &mut vec![(
                    Event::SweepSuccess(SweepSuccess {
                        id: sweep_address.id,
                        txids,
                    }),
                    self.tasks_sources
                        .get(id)
                        .cloned()
                        .expect("task source missing"),
                )],
            )
            .await;
            if let Some(ids) = self.lifetimes.get_mut(&sweep_address.lifetime) {
                ids.remove(id);
                if ids.is_empty() {
                    self.lifetimes.remove(&sweep_address.lifetime);
                }
            }
            self.sweep_addresses.remove(id);
            self.tasks_sources.remove(id);
        }
    }

    pub async fn fee_estimated(&mut self, fee_estimations: FeeEstimations) {
        // Emit fee estimation events
        if self.fee_estimation.as_ref() != Some(&fee_estimations) {
            for (id, task) in self.watch_fee_estimation.iter() {
                send_event(
                    &self.tx_event,
                    &mut vec![(
                        Event::FeeEstimation(FeeEstimation {
                            id: task.id,
                            fee_estimations: fee_estimations.clone(),
                        }),
                        self.tasks_sources.get(id).unwrap().clone(),
                    )],
                )
                .await;
            }

            self.fee_estimation = Some(fee_estimations);
        }
        self.drop_lifetimes();
    }

    pub async fn fail_sweep(&mut self, id: &InternalId) {
        if let Some(sweep_address) = self.sweep_addresses.get(id) {
            send_event(
                &self.tx_event,
                &mut vec![(
                    Event::TaskAborted(TaskAborted {
                        id: vec![],
                        error: Some(
                            "Sweep failed, did not find any assets associated with the address"
                                .to_string(),
                        ),
                    }),
                    self.tasks_sources
                        .get(id)
                        .cloned()
                        .expect("task source missing"),
                )],
            )
            .await;
            if let Some(ids) = self.lifetimes.get_mut(&sweep_address.lifetime) {
                ids.remove(id);
                if ids.is_empty() {
                    self.lifetimes.remove(&sweep_address.lifetime);
                }
            }
            self.sweep_addresses.remove(id);
            self.tasks_sources.remove(id);
        }
    }

    fn drop_lifetimes(&mut self) {
        let lifetimes: Vec<u64> = Iterator::collect(self.lifetimes.keys().map(|&x| x.to_owned()));
        for lifetime in lifetimes {
            if lifetime < self.block_height {
                warn!("dropping lifetime");
                self.drop_lifetime(lifetime);
            }
        }
    }

    fn add_lifetime(&mut self, lifetime: u64, id: InternalId) -> Result<(), Error> {
        if lifetime < self.block_height {
            return Err(Error::Farcaster("task lifetime expired".to_string()));
        }

        if let Some(lifetimes) = self.lifetimes.get_mut(&lifetime) {
            lifetimes.insert(id);
        } else {
            let mut lifetimes = HashSet::new();
            lifetimes.insert(id);
            self.lifetimes.insert(lifetime, lifetimes);
        }
        Ok(())
    }

    fn drop_lifetime(&mut self, lifetime: u64) {
        if let Some(tasks) = self.lifetimes.remove(&lifetime) {
            for task in &tasks {
                self.addresses.remove(task);
                self.transactions.remove(task);
                self.unseen_transactions.remove(task);
                self.watch_height.remove(task);
                self.watch_fee_estimation.remove(task);
                self.sweep_addresses.remove(task);
                self.tasks_sources.remove(task);
            }
        } else {
            error!("Unknown lifetime {}", lifetime);
        }
    }

    fn remove_transaction(&mut self, id: &InternalId) {
        if let Some(watch_transactions) = self.transactions.get(id) {
            if let Some(ids) = self.lifetimes.get_mut(&watch_transactions.task.lifetime) {
                ids.remove(id);
                if ids.is_empty() {
                    self.lifetimes.remove(&watch_transactions.task.lifetime);
                }
            }
        }
        self.transactions.remove(id);
        self.unseen_transactions.remove(id);
        self.tasks_sources.remove(id);
    }

    fn remove_address(&mut self, id: &InternalId) {
        if let Some(address_transactions) = self.addresses.get(id) {
            if let Some(ids) = self.lifetimes.get_mut(&address_transactions.task.lifetime) {
                ids.remove(id);
                if ids.is_empty() {
                    self.lifetimes.remove(&address_transactions.task.lifetime);
                }
            }
        }
        self.addresses.remove(id);
        self.tasks_sources.remove(id);
    }

    fn remove_height(&mut self, id: &InternalId) {
        if let Some(watch_height) = self.watch_height.get(id) {
            if let Some(ids) = self.lifetimes.get_mut(&watch_height.lifetime) {
                ids.remove(id);
                if ids.is_empty() {
                    self.lifetimes.remove(&watch_height.lifetime);
                }
            }
        }
        self.watch_height.remove(id);
        self.tasks_sources.remove(id);
    }

    fn remove_sweep_address(&mut self, id: &InternalId) {
        if let Some(sweep_address) = self.sweep_addresses.get(id) {
            if let Some(ids) = self.lifetimes.get_mut(&sweep_address.lifetime) {
                ids.remove(id);
                if ids.is_empty() {
                    self.lifetimes.remove(&sweep_address.lifetime);
                }
            }
        }
        self.sweep_addresses.remove(id);
        self.tasks_sources.remove(id);
    }
}

async fn send_event(
    tx_event: &TokioSender<SyncerdBridgeEvent>,
    events: &mut Vec<(Event, ServiceId)>,
) {
    for (event, source) in events.drain(..) {
        tx_event
            .send(SyncerdBridgeEvent { event, source })
            .await
            .expect("error sending event from the syncer state");
    }
}

#[tokio::test]
async fn syncer_state_transaction() {
    use tokio::sync::mpsc::Receiver as TokioReceiver;
    let (event_tx, mut event_rx): (
        TokioSender<SyncerdBridgeEvent>,
        TokioReceiver<SyncerdBridgeEvent>,
    ) = tokio::sync::mpsc::channel(120);
    let mut state = SyncerState::new(event_tx.clone(), Blockchain::Bitcoin);

    let transaction_task_one = WatchTransaction {
        id: TaskId(0),
        lifetime: 1,
        hash: vec![0],
        confirmation_bound: 4,
    };
    let transaction_task_two = WatchTransaction {
        id: TaskId(0),
        lifetime: 3,
        hash: vec![1],
        confirmation_bound: 4,
    };
    let height_task = WatchHeight {
        id: TaskId(0),
        lifetime: 4,
    };
    let source1 = ServiceId::Syncer(Blockchain::Bitcoin, Network::Mainnet);

    state.watch_transaction(transaction_task_one.clone(), source1.clone());
    state
        .abort(TaskTarget::TaskId(TaskId(0)), source1.clone(), true)
        .await;
    assert!(event_rx.try_recv().is_ok());

    state.watch_transaction(transaction_task_one.clone(), source1.clone());
    state.watch_transaction(transaction_task_two.clone(), source1.clone());
    state.watch_height(height_task, source1.clone()).await;
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.unseen_transactions.len(), 2);
    assert!(event_rx.try_recv().is_err());

    state
        .change_transaction(vec![0], none!(), none!(), none!())
        .await;
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.unseen_transactions.len(), 2);
    assert!(event_rx.try_recv().is_ok());

    state
        .change_transaction(vec![0], none!(), Some(0), none!())
        .await;
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.unseen_transactions.len(), 1);
    assert!(event_rx.try_recv().is_ok());

    state
        .change_transaction(vec![0], none!(), Some(0), none!())
        .await;
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.unseen_transactions.len(), 1);
    assert!(event_rx.try_recv().is_err());

    state
        .change_transaction(vec![0], Some(vec![1]), Some(1), none!())
        .await;
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.unseen_transactions.len(), 1);
    assert!(event_rx.try_recv().is_ok());

    state
        .change_transaction(vec![0], Some(vec![1]), Some(1), none!())
        .await;
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.unseen_transactions.len(), 1);
    assert!(event_rx.try_recv().is_err());

    state
        .change_transaction(vec![0], none!(), none!(), none!())
        .await;
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.unseen_transactions.len(), 2);
    assert!(event_rx.try_recv().is_ok());

    let source2 = ServiceId::Syncer(Blockchain::Monero, Network::Mainnet);
    state.watch_transaction(transaction_task_two.clone(), source2.clone());
    state
        .abort(TaskTarget::TaskId(TaskId(0)), source2.clone(), true)
        .await;
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.unseen_transactions.len(), 2);
    assert!(event_rx.try_recv().is_ok());

    state.change_height(5, vec![1]).await;
    assert_eq!(state.lifetimes.len(), 0);
    assert_eq!(state.transactions.len(), 0);
    assert_eq!(state.tasks_sources.len(), 0);
    assert_eq!(state.unseen_transactions.len(), 0);
    assert!(event_rx.try_recv().is_err());
}

#[tokio::test]
async fn syncer_state_addresses() {
    use std::str::FromStr;
    use tokio::sync::mpsc::Receiver as TokioReceiver;
    let (event_tx, mut event_rx): (
        TokioSender<SyncerdBridgeEvent>,
        TokioReceiver<SyncerdBridgeEvent>,
    ) = tokio::sync::mpsc::channel(120);
    let mut state = SyncerState::new(event_tx.clone(), Blockchain::Bitcoin);
    let address = bitcoin::Address::from_str("32BkaQeAVcd65Vn7pjEziohf5bCiryNQov").unwrap();
    let addendum = AddressAddendum::Bitcoin(BtcAddressAddendum {
        from_height: 0,
        address,
    });
    let address_task = WatchAddress {
        id: TaskId(0),
        lifetime: 1,
        addendum: addendum.clone(),
        include_tx: Boolean::False,
    };
    let address_task_two = WatchAddress {
        id: TaskId(0),
        lifetime: 1,
        addendum: addendum.clone(),
        include_tx: Boolean::False,
    };
    let source1 = ServiceId::Syncer(Blockchain::Bitcoin, Network::Mainnet);

    state.watch_address(address_task_two.clone(), source1.clone());
    state
        .abort(TaskTarget::TaskId(TaskId(0)), source1.clone(), true)
        .await;
    assert_eq!(state.lifetimes.len(), 0);
    assert_eq!(state.tasks_sources.len(), 0);
    assert_eq!(state.addresses.len(), 0);
    assert!(event_rx.try_recv().is_ok());

    state.watch_address(address_task, source1.clone());
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.addresses.len(), 1);
    let address_tx_one = AddressTx {
        our_amount: 1,
        tx_id: vec![0; 32],
        tx: vec![0],
    };
    let address_tx_two = AddressTx {
        our_amount: 1,
        tx_id: vec![1; 32],
        tx: vec![0],
    };
    let address_tx_three = AddressTx {
        our_amount: 1,
        tx_id: vec![2; 32],
        tx: vec![0],
    };
    let address_tx_four = AddressTx {
        our_amount: 1,
        tx_id: vec![3; 32],
        tx: vec![0],
    };

    state
        .change_address(addendum.clone(), create_set(vec![address_tx_one.clone()]))
        .await;
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.addresses.len(), 1);
    assert!(event_rx.try_recv().is_ok());

    state
        .change_address(addendum.clone(), create_set(vec![address_tx_one.clone()]))
        .await;
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.addresses.len(), 1);
    assert!(event_rx.try_recv().is_err());

    state
        .change_address(
            addendum.clone(),
            create_set(vec![address_tx_one.clone(), address_tx_one.clone()]),
        )
        .await;
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.addresses.len(), 1);
    assert!(event_rx.try_recv().is_err());

    state
        .change_address(
            addendum.clone(),
            create_set(vec![address_tx_one.clone(), address_tx_two.clone()]),
        )
        .await;
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.addresses.len(), 1);
    assert!(event_rx.try_recv().is_ok());

    state
        .change_address(
            addendum.clone(),
            create_set(vec![address_tx_one.clone(), address_tx_two.clone()]),
        )
        .await;
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.addresses.len(), 1);
    assert!(event_rx.try_recv().is_err());

    state
        .change_address(
            addendum.clone(),
            create_set(vec![address_tx_three.clone(), address_tx_four.clone()]),
        )
        .await;
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.addresses.len(), 1);
    assert!(event_rx.try_recv().is_ok());
    assert!(event_rx.try_recv().is_ok());

    let source2 = ServiceId::Syncer(Blockchain::Monero, Network::Testnet);
    state.watch_address(address_task_two.clone(), source2.clone());
    state
        .abort(TaskTarget::TaskId(TaskId(0)), source2.clone(), true)
        .await;
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.addresses.len(), 1);
    assert!(event_rx.try_recv().is_ok());

    state.change_height(1, vec![1]).await;
    let height_task = WatchHeight {
        id: TaskId(0),
        lifetime: 3,
    };
    state.watch_height(height_task, source1.clone()).await;
    assert_eq!(state.lifetimes.len(), 2);
    assert_eq!(state.tasks_sources.len(), 2);
    assert!(event_rx.try_recv().is_ok());

    state.change_height(2, vec![2]).await;
    state
        .change_address(
            addendum.clone(),
            create_set(vec![address_tx_one.clone(), address_tx_two.clone()]),
        )
        .await;
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.addresses.len(), 0);
    assert!(event_rx.try_recv().is_ok());
}

#[tokio::test]
async fn syncer_state_sweep_addresses() {
    use std::str::FromStr;
    use tokio::sync::mpsc::Receiver as TokioReceiver;
    let (event_tx, mut event_rx): (
        TokioSender<SyncerdBridgeEvent>,
        TokioReceiver<SyncerdBridgeEvent>,
    ) = tokio::sync::mpsc::channel(120);
    let mut state = SyncerState::new(event_tx.clone(), Blockchain::Monero);
    let sweep_task = SweepAddress {
        id: TaskId(0),
        lifetime: 11,
        retry: true,
        from_height: None,
        addendum: SweepAddressAddendum::Monero(SweepMoneroAddress {
            source_view_key: monero::PrivateKey::from_str(
                "77916d0cd56ed1920aef6ca56d8a41bac915b68e4c46a589e0956e27a7b77404",
            )
            .unwrap(),
            source_spend_key: monero::PrivateKey::from_str(
                "77916d0cd56ed1920aef6ca56d8a41bac915b68e4c46a589e0956e27a7b77404",
            )
            .unwrap(),
            destination_address: monero::Address::from_str(
                "51qzspbPiQ9Z9Wq3hR8HRhPmVcE3URCK8b8A9ypHHzyvhigWTefCapoG1MXVZQQi7B5t4DpJYrHZyaFjHSb5QqLe8YEaBpo"
            )
            .unwrap(),
            minimum_balance: monero::Amount::from_pico(1),
        }),
    };
    let source1 = ServiceId::Syncer(Blockchain::Monero, Network::Mainnet);

    state.sweep_address(sweep_task.clone(), source1.clone());
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.sweep_addresses.len(), 1);
    state
        .abort(TaskTarget::TaskId(TaskId(0)), source1.clone(), true)
        .await;
    assert_eq!(state.lifetimes.len(), 0);
    assert_eq!(state.tasks_sources.len(), 0);
    assert_eq!(state.sweep_addresses.len(), 0);
    assert!(event_rx.try_recv().is_ok());

    state.sweep_address(sweep_task, source1.clone());
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.sweep_addresses.len(), 1);
    state.success_sweep(&InternalId(2), vec![vec![0]]).await;
    assert_eq!(state.lifetimes.len(), 0);
    assert_eq!(state.tasks_sources.len(), 0);
    assert_eq!(state.sweep_addresses.len(), 0);
    assert!(event_rx.try_recv().is_ok());
}

#[tokio::test]
async fn syncer_state_height() {
    use tokio::sync::mpsc::Receiver as TokioReceiver;
    let (event_tx, mut event_rx): (
        TokioSender<SyncerdBridgeEvent>,
        TokioReceiver<SyncerdBridgeEvent>,
    ) = tokio::sync::mpsc::channel(120);
    let mut state = SyncerState::new(event_tx.clone(), Blockchain::Bitcoin);
    let height_task = WatchHeight {
        id: TaskId(0),
        lifetime: 0,
    };
    let another_height_task = WatchHeight {
        id: TaskId(0),
        lifetime: 3,
    };
    let source1 = ServiceId::Syncer(Blockchain::Bitcoin, Network::Mainnet);

    state
        .watch_height(height_task.clone(), source1.clone())
        .await;
    state
        .abort(TaskTarget::TaskId(TaskId(0)), source1.clone(), true)
        .await;
    assert_eq!(state.lifetimes.len(), 0);
    assert_eq!(state.tasks_sources.len(), 0);
    assert_eq!(state.watch_height.len(), 0);
    assert!(event_rx.try_recv().is_ok());

    state
        .watch_height(height_task.clone(), source1.clone())
        .await;
    state
        .watch_height(another_height_task.clone(), source1.clone())
        .await;
    assert_eq!(state.lifetimes.len(), 2);
    assert_eq!(state.tasks_sources.len(), 2);
    assert_eq!(state.watch_height.len(), 2);
    assert!(event_rx.try_recv().is_err());

    state.change_height(1, vec![1]).await;
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.watch_height.len(), 1);
    assert!(event_rx.try_recv().is_ok());

    state.change_height(3, vec![3]).await;
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.watch_height.len(), 1);
    assert!(event_rx.try_recv().is_ok());

    state.change_height(3, vec![3]).await;
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.watch_height.len(), 1);
    assert!(event_rx.try_recv().is_err());

    let source2 = ServiceId::Syncer(Blockchain::Monero, Network::Mainnet);
    state
        .watch_height(another_height_task.clone(), source2.clone())
        .await;
    state
        .abort(TaskTarget::TaskId(TaskId(0)), source2.clone(), true)
        .await;
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.watch_height.len(), 1);
    assert!(event_rx.try_recv().is_ok());
    assert!(event_rx.try_recv().is_ok());

    state.change_height(4, vec![0]).await;
    assert_eq!(state.lifetimes.len(), 0);
    assert_eq!(state.tasks_sources.len(), 0);
    assert_eq!(state.watch_height.len(), 0);
    assert!(event_rx.try_recv().is_err());
}
