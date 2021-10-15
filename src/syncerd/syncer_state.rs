use crate::farcaster_core::consensus::Decodable;
use crate::rpc::request::SyncerdBridgeEvent;
use crate::syncerd::opts::Coin;
use crate::syncerd::runtime::SyncerdTask;
use crate::Error;
use crate::ServiceId;
use bitcoin::{hashes::Hash, Txid};
use microservices::rpc_connection::Request;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::io;
use std::marker::{Send, Sized};
use tokio::sync::mpsc::Sender as TokioSender;

use crate::serde::Deserialize;
use crate::syncerd::*;
use hex;

pub struct SyncerState {
    block_height: u64,
    block_hash: Vec<u8>,
    tasks_sources: HashMap<u32, ServiceId>,
    watch_height: HashMap<u32, WatchHeight>,
    lifetimes: HashMap<u64, HashSet<u32>>,
    pub addresses: HashMap<u32, AddressTransactions>,
    pub transactions: HashMap<u32, WatchedTransaction>,
    pub unseen_transactions: HashSet<u32>,
    tx_event: TokioSender<SyncerdBridgeEvent>,
    task_count: u32,
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
    pub fn new(tx_event: TokioSender<SyncerdBridgeEvent>) -> Self {
        Self {
            block_height: 0,
            block_hash: vec![0],
            tasks_sources: HashMap::new(),
            watch_height: HashMap::new(),
            lifetimes: HashMap::new(),
            addresses: HashMap::new(),
            transactions: HashMap::new(),
            unseen_transactions: HashSet::new(),
            tx_event,
            task_count: 0,
        }
    }

    pub async fn abort(&mut self, task_id: u32, source: ServiceId) {
        let status = 0;
        // match self.tasks.remove(&task.id) {
        //     None => {}
        //     Some(task) => {
        //         match task {
        //             Task::Abort(_abort) => {}
        //             Task::WatchHeight(height) => {
        //                 self.remove_watch_height(height.lifetime, height.id);
        //             }
        //             Task::WatchAddress(address) => {
        //                 self.remove_address(address.lifetime, address.id);
        //             }
        //             Task::WatchTransaction(tx) => {
        //                 self.remove_transaction(tx.lifetime, tx.id);
        //             }

        //             // Just set an error code as we immediately attempt to broadcast
        // a transaction when told to
        // Task::BroadcastTransaction(_tx) => {                 status = 1;
        //             }
        //         }
        //     }
        // }

        // Emit the task_aborted event
        send_event(
            &self.tx_event,
            &mut vec![(
                Event::TaskAborted(TaskAborted {
                    id: task_id,
                    success_abort: status,
                }),
                source,
            )],
        )
        .await;
    }

    pub async fn watch_height(&mut self, task: WatchHeight, source: ServiceId) {
        // increment the count to use it as a unique internal id
        self.task_count += 1;
        // This is technically valid behavior; immediately prune the task for being past
        // its lifetime by never inserting it
        if let Err(e) = self.add_lifetime(task.lifetime, self.task_count) {
            error!("{}", e);
            return;
        }
        self.watch_height.insert(self.task_count, task.clone());
        self.tasks_sources.insert(self.task_count, source.clone());

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

    pub fn watch_address(&mut self, task: WatchAddress, source: ServiceId) -> Result<(), Error> {
        // increment the count to use it as a unique internal id
        self.task_count += 1;
        self.add_lifetime(task.lifetime, self.task_count)?;
        self.tasks_sources.insert(self.task_count, source);
        let address_txs = AddressTransactions {
            task,
            known_txs: none!(),
        };
        self.addresses.insert(self.task_count, address_txs);
        Ok(())
    }

    pub fn watch_transaction(&mut self, task: WatchTransaction, source: ServiceId) {
        // increment the count to use it as a unique internal id
        self.task_count += 1;

        if let Err(e) = self.add_lifetime(task.lifetime, self.task_count) {
            error!("{}", e);
            return;
        };
        self.tasks_sources.insert(self.task_count, source);
        self.transactions.insert(
            self.task_count,
            WatchedTransaction {
                task: task.clone(),
                transaction_confirmations: TransactionConfirmations {
                    id: task.id,
                    block: none!(),
                    confirmations: None,
                },
            },
        );
        self.unseen_transactions.insert(self.task_count);
    }

    pub async fn change_height(&mut self, height: u64, block: Vec<u8>) {
        if self.block_height != height || self.block_hash != block {
            self.block_height = height;
            self.block_hash = block.clone();

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
            addresses: &mut HashMap<u32, AddressTransactions>,
            events: &mut Vec<(Event, ServiceId)>,
            tasks_sources: &mut HashMap<u32, ServiceId>,
            address_addendum: AddressAddendum,
            txs: HashSet<AddressTx>,
        ) {
            let changes_detected: bool = addresses
                .iter()
                .find_map(|(_, addr)| {
                    let new_txs = txs.difference(&addr.known_txs).collect::<Vec<&_>>();
                    if !new_txs.is_empty() {
                        Some(true)
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| false);

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
                        info!("new_tx seen: {}", Txid::from_slice(&new_tx.tx_id).unwrap());
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
                            task: addr.task.clone(),
                            known_txs: txs.clone(),
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
        );

        fn inner(
            transactions: &mut HashMap<u32, WatchedTransaction>,
            unseen_transactions: &mut HashSet<u32>,
            events: &mut Vec<(Event, ServiceId)>,
            tasks_sources: &mut HashMap<u32, ServiceId>,
            tx_id: Vec<u8>,
            block_hash: Option<Vec<u8>>,
            confirmations: Option<u32>,
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
                        return Some((id.clone(), watched_tx.clone()));
                    }
                    if confirmations.is_some() {
                        unseen_transactions.remove(&id);
                    } else {
                        if !unseen_transactions.contains(&id) {
                            unseen_transactions.insert(id.clone());
                        }
                    }
                    let transaction_confirmations = if confirmations
                        != watched_tx.transaction_confirmations.confirmations
                        || block != watched_tx.transaction_confirmations.block
                    {
                        let tx_confs = TransactionConfirmations {
                            id: watched_tx.task.id,
                            block: block.clone(),
                            confirmations,
                        };
                        events.push((
                            Event::TransactionConfirmations(tx_confs.clone()),
                            tasks_sources.get(&id).unwrap().clone(),
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
                            id.clone(),
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

    fn remove_transaction(&mut self, transaction_lifetime: u64, id: u32) {
        self.remove_lifetime(transaction_lifetime, id);
        self.transactions.remove(&id);
        self.unseen_transactions.remove(&id);
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

    fn add_lifetime(&mut self, lifetime: u64, id: u32) -> Result<(), Error> {
        if lifetime < self.block_height {
            Err(Error::Farcaster("task lifetime expired".to_string()))?;
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

    fn remove_lifetime(&mut self, lifetime: u64, id: u32) {
        if let Some(lifetimes) = self.lifetimes.get_mut(&lifetime) {
            lifetimes.remove(&id);
        }
    }

    fn drop_lifetime(&mut self, lifetime: u64) {
        if let Some(tasks) = self.lifetimes.remove(&lifetime) {
            for task in &tasks {
                self.addresses.remove(task);
                self.transactions.remove(task);
                self.unseen_transactions.remove(task);
                self.watch_height.remove(task);
                self.tasks_sources.remove(task);
            }
        } else {
            error!("Unknown lifetime {}", lifetime);
        }
    }

    fn remove_address(&mut self, address_lifetime: u64, id: u32) {
        self.remove_lifetime(address_lifetime, id);
        self.addresses.remove(&id);
    }

    fn remove_watch_height(&mut self, height_lifetime: u64, id: u32) {
        self.remove_lifetime(height_lifetime, id);
        self.watch_height.remove(&id);
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
    let mut state = SyncerState::new(event_tx.clone());

    let transaction_task_one = WatchTransaction {
        id: 0,
        lifetime: 1,
        hash: vec![0],
        confirmation_bound: 4,
    };
    let transaction_task_two = WatchTransaction {
        id: 0,
        lifetime: 3,
        hash: vec![1],
        confirmation_bound: 4,
    };
    let height_task = WatchHeight { id: 0, lifetime: 4 };
    state.watch_transaction(transaction_task_one, ServiceId::Syncer(Coin::Bitcoin));
    state.watch_transaction(transaction_task_two, ServiceId::Syncer(Coin::Bitcoin));
    state
        .watch_height(height_task, ServiceId::Syncer(Coin::Bitcoin))
        .await;
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.unseen_transactions.len(), 2);
    assert!(event_rx.try_recv().is_err());

    state.change_transaction(vec![0], none!(), none!()).await;
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.unseen_transactions.len(), 2);
    assert!(event_rx.try_recv().is_ok());

    state.change_transaction(vec![0], none!(), Some(0)).await;
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.unseen_transactions.len(), 1);
    assert!(event_rx.try_recv().is_ok());

    state.change_transaction(vec![0], none!(), Some(0)).await;
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.unseen_transactions.len(), 1);
    assert!(event_rx.try_recv().is_err());

    state
        .change_transaction(vec![0], Some(vec![1]), Some(1))
        .await;
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.unseen_transactions.len(), 1);
    assert!(event_rx.try_recv().is_ok());

    state
        .change_transaction(vec![0], Some(vec![1]), Some(1))
        .await;
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.unseen_transactions.len(), 1);
    assert!(event_rx.try_recv().is_err());

    state.change_transaction(vec![0], none!(), none!()).await;
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.unseen_transactions.len(), 2);
    assert!(event_rx.try_recv().is_ok());

    state.change_height(5, vec![0]).await;
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
    let mut state = SyncerState::new(event_tx.clone());
    let address = bitcoin::Address::from_str("32BkaQeAVcd65Vn7pjEziohf5bCiryNQov").unwrap();
    let addendum = AddressAddendum::Bitcoin(BtcAddressAddendum {
        address: Some(address.clone()),
        from_height: 0,
        script_pubkey: address.script_pubkey(),
    });
    let address_task = WatchAddress {
        id: 0,
        lifetime: 1,
        addendum: addendum.clone(),
        include_tx: Boolean::False,
    };
    state
        .watch_address(address_task, ServiceId::Syncer(Coin::Bitcoin))
        .unwrap();
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

    state.change_height(1, vec![0]).await;
    let height_task = WatchHeight { id: 0, lifetime: 3 };
    state
        .watch_height(height_task, ServiceId::Syncer(Coin::Bitcoin))
        .await;
    assert_eq!(state.lifetimes.len(), 2);
    assert_eq!(state.tasks_sources.len(), 2);
    assert!(event_rx.try_recv().is_ok());

    state.change_height(2, vec![0]).await;
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
async fn syncer_state_height() {
    use tokio::sync::mpsc::Receiver as TokioReceiver;
    let (event_tx, mut event_rx): (
        TokioSender<SyncerdBridgeEvent>,
        TokioReceiver<SyncerdBridgeEvent>,
    ) = tokio::sync::mpsc::channel(120);
    let mut state = SyncerState::new(event_tx.clone());
    let height_task = WatchHeight { id: 0, lifetime: 0 };
    let another_height_task = WatchHeight { id: 0, lifetime: 3 };

    state
        .watch_height(height_task, ServiceId::Syncer(Coin::Bitcoin))
        .await;
    state
        .watch_height(another_height_task, ServiceId::Syncer(Coin::Bitcoin))
        .await;
    assert_eq!(state.lifetimes.len(), 2);
    assert_eq!(state.tasks_sources.len(), 2);
    assert_eq!(state.watch_height.len(), 2);
    assert!(event_rx.try_recv().is_err());

    state.change_height(1, vec![0]).await;
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.watch_height.len(), 1);
    assert!(event_rx.try_recv().is_ok());

    state.change_height(3, vec![0]).await;
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.watch_height.len(), 1);
    assert!(event_rx.try_recv().is_ok());

    state.change_height(3, vec![0]).await;
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.watch_height.len(), 1);
    assert!(event_rx.try_recv().is_err());

    state.change_height(4, vec![0]).await;
    assert_eq!(state.lifetimes.len(), 0);
    assert_eq!(state.tasks_sources.len(), 0);
    assert_eq!(state.watch_height.len(), 0);
    assert!(event_rx.try_recv().is_err());
    return;
}
