use crate::farcaster_core::consensus::Decodable;
use crate::syncerd::opts::Coin;
use crate::ServiceId;
use microservices::rpc_connection::Request;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::io;
use std::marker::{Send, Sized};

use crate::serde::Deserialize;
use hex;

use farcaster_core::bitcoin::tasks::BtcAddressAddendum;
use farcaster_core::syncer::*;

pub struct SyncerState {
    block_height: u64,
    block_hash: Vec<u8>,
    tasks_sources: HashMap<u32, ServiceId>,
    watch_height: HashMap<u32, WatchHeight>,
    lifetimes: HashMap<u64, HashSet<u32>>,
    pub addresses: HashMap<u32, AddressTransactions>,
    pub transactions: HashMap<u32, WatchedTransaction>,
    pub events: Vec<(Event, ServiceId)>,
    task_count: u32,
}

#[derive(Clone, Debug)]
pub struct WatchedTransaction {
    pub task: WatchTransaction,
    pub transaction_confirmations: TransactionConfirmations,
}

#[derive(Clone, Eq, PartialEq)]
pub struct AddressTransactions {
    pub task: WatchAddress,
    txs: HashMap<Vec<u8>, AddressTx>,
}

#[derive(Debug, Clone, PartialEq, Hash, Eq)]
pub struct AddressTx {
    pub our_amount: u64,
    pub tx_id: Vec<u8>,
    pub block_hash: Vec<u8>,
    pub tx: Vec<u8>,
}

pub fn txid_tx_hashmap(addrs: Vec<AddressTx>) -> HashMap<Vec<u8>, AddressTx> {
    addrs.into_iter().map(|a| (a.tx_id.clone(), a)).collect()
}

impl SyncerState {
    pub fn new() -> Self {
        Self {
            block_height: 0,
            block_hash: vec![0],
            tasks_sources: HashMap::new(),
            watch_height: HashMap::new(),
            lifetimes: HashMap::new(),
            addresses: HashMap::new(),
            transactions: HashMap::new(),
            events: vec![],
            task_count: 0,
        }
    }

    pub fn abort(&mut self, task_id: i32, source: ServiceId) {
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

        //             // Just set an error code as we immediately attempt to broadcast a transaction when told to
        //             Task::BroadcastTransaction(_tx) => {
        //                 status = 1;
        //             }
        //         }
        //     }
        // }

        // Emit the task_aborted event
        self.events.push((
            Event::TaskAborted(TaskAborted {
                id: task_id,
                success_abort: status,
            }),
            source,
        ));
    }

    pub fn watch_height(&mut self, task: WatchHeight, source: ServiceId) {
        // increment the count to use it as a unique internal id
        self.task_count += 1;
        // This is technically valid behavior; immediately prune the task for being past its lifetime by never inserting it
        if !self.add_lifetime(task.lifetime, self.task_count).is_ok() {
            return;
        }
        self.watch_height.insert(self.task_count, task.clone());
        self.tasks_sources.insert(self.task_count, source.clone());
        self.events.push((
            Event::HeightChanged(HeightChanged {
                id: task.id,
                block: self.block_hash.clone(),
                height: self.block_height,
            }),
            source,
        ));
    }

    pub fn watch_address(&mut self, task: WatchAddress, source: ServiceId) -> Result<(), Error> {
        // increment the count to use it as a unique internal id
        self.task_count += 1;
        self.add_lifetime(task.lifetime, self.task_count)?; // FIXME turned off because it errors here
        self.tasks_sources.insert(self.task_count, source);
        let address_tx = AddressTransactions { task, txs: none!() };
        self.addresses.insert(self.task_count, address_tx);
        Ok(())
    }

    pub fn watch_transaction(&mut self, task: WatchTransaction, source: ServiceId) {
        // increment the count to use it as a unique internal id
        self.task_count += 1;

        if self.add_lifetime(task.lifetime, self.task_count).is_err() {
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
                    confirmations: -1,
                },
            },
        );
    }

    pub fn change_height(&mut self, height: u64, block: Vec<u8>) {
        if self.block_height != height || self.block_hash != block {
            self.block_height = height;
            self.block_hash = block.clone();

            self.drop_lifetimes();

            // Emit a height_changed event
            for (id, task) in self.watch_height.iter() {
                self.events.push((
                    Event::HeightChanged(HeightChanged {
                        id: task.id,
                        block: block.clone(),
                        height: self.block_height,
                    }),
                    self.tasks_sources.get(id).unwrap().clone(),
                ));
            }
        }
    }

    pub fn change_address(&mut self, address_addendum: Vec<u8>, txs: HashMap<Vec<u8>, AddressTx>) {
        trace!("inside change_address");
        self.drop_lifetimes();
        if self.addresses.is_empty() {
            debug!("no addresses here");
        }
        self.addresses = self
            .addresses
            .clone()
            .iter()
            .filter(|(_, addr)| addr.task.addendum == address_addendum && addr.txs != txs)
            .map(|(id, addr)| {
                for (_, tx) in txs
                    .iter()
                    .find(|&(tx_id, _)| !addr.txs.contains_key(tx_id))
                {
                    let address_transaction = AddressTransaction {
                        id: addr.task.id,
                        hash: tx.tx_id.clone(),
                        amount: tx.our_amount,
                        block: tx.block_hash.clone(),
                        tx: tx.tx.clone(),
                    };
                    self.events.push((
                        Event::AddressTransaction(address_transaction.clone()),
                        self.tasks_sources.get(id).unwrap().clone(),
                    ));
                }

                (
                    id.clone(),
                    AddressTransactions {
                        task: addr.task.clone(),
                        txs: txs.clone(),
                    },
                )
            })
            .collect();
    }

    pub fn change_transaction(
        &mut self,
        tx_id: Vec<u8>,
        block_hash: Option<Vec<u8>>,
        confirmations: Option<u32>,
    ) {
        self.drop_lifetimes();

        let block = match block_hash {
            Some(bh) => bh,
            // per RFC, no block hash should be encoded as 0x0
            None => hex::decode("00").unwrap(),
        };
        let confs = match confirmations {
            Some(confs) => confs as i32,
            // per RFC, no confirmation should be encoded as -1
            None => -1,
        };

        self.transactions = self
            .transactions
            .clone()
            .iter()
            .map(|(id, watched_tx)| {
                if watched_tx.task.hash == tx_id
                    && (watched_tx.transaction_confirmations.block != block
                        || watched_tx.transaction_confirmations.confirmations != confs)
                {
                    let transaction_confirmations = TransactionConfirmations {
                        id: watched_tx.task.id,
                        block: block.clone(),
                        confirmations: confs,
                    };
                    self.events.push((
                        Event::TransactionConfirmations(transaction_confirmations.clone()),
                        self.tasks_sources.get(id).unwrap().clone(),
                    ));
                    // prune the task once it has reached its confirmation bound
                    if confs >= watched_tx.task.confirmation_bound as i32 {
                        self.remove_transaction(watched_tx.task.lifetime, *id);
                    }
                    return (
                        id.clone(),
                        WatchedTransaction {
                            task: watched_tx.task.clone(),
                            transaction_confirmations: transaction_confirmations.clone(),
                        },
                    );
                }
                return (id.clone(), watched_tx.clone());
            })
            .collect();
    }

    fn remove_transaction(&mut self, transaction_lifetime: u64, id: u32) {
        self.remove_lifetime(transaction_lifetime, id);
        self.transactions.remove(&id);
    }

    fn drop_lifetimes(&mut self) {
        let lifetimes: Vec<u64> = Iterator::collect(self.lifetimes.keys().map(|&x| x.to_owned()));
        for lifetime in lifetimes {
            if lifetime < self.block_height {
                self.drop_lifetime(lifetime);
            }
        }
    }

    fn add_lifetime(&mut self, lifetime: u64, id: u32) -> Result<(), Error> {
        if lifetime < self.block_height {
            Err(Error::new(Error::LifetimeExpired))?;
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
        for task in self.lifetimes.remove(&lifetime).unwrap().iter() {
            self.addresses.remove(task);
            self.transactions.remove(task);
            self.watch_height.remove(task);
            self.tasks_sources.remove(task);
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

#[test]
fn syncer_state_transaction() {
    let mut state = SyncerState::new();
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
    let height_task = WatchHeight {
        id: 0,
        lifetime: 4,
        addendum: vec![],
    };
    state.watch_transaction(transaction_task_one, ServiceId::Syncer(Coin::Bitcoin));
    state.watch_transaction(transaction_task_two, ServiceId::Syncer(Coin::Bitcoin));
    state.watch_height(height_task, ServiceId::Syncer(Coin::Bitcoin));
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.events.len(), 1);

    state.change_transaction(vec![0], none!(), none!());
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.events.len(), 2);

    state.change_transaction(vec![0], none!(), Some(0));
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.events.len(), 3);

    state.change_transaction(vec![0], none!(), Some(0));
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.events.len(), 3);

    state.change_transaction(vec![0], Some(vec![1]), Some(1));
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.events.len(), 4);

    state.change_transaction(vec![0], Some(vec![1]), Some(1));
    assert_eq!(state.lifetimes.len(), 3);
    assert_eq!(state.transactions.len(), 2);
    assert_eq!(state.tasks_sources.len(), 3);
    assert_eq!(state.events.len(), 4);

    state.change_height(5, vec![0]);
    assert_eq!(state.lifetimes.len(), 0);
    assert_eq!(state.transactions.len(), 0);
    assert_eq!(state.tasks_sources.len(), 0);
    assert_eq!(state.events.len(), 4);
}

#[test]
fn syncer_state_addresses() {
    let mut state = SyncerState::new();
    let address_task = WatchAddress {
        id: 0,
        lifetime: 1,
        addendum: vec![0],
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
        block_hash: vec![0],
        tx: vec![0],
    };
    let address_tx_two = AddressTx {
        our_amount: 1,
        tx_id: vec![1; 32],
        block_hash: vec![0],
        tx: vec![0],
    };
    let address_tx_three = AddressTx {
        our_amount: 1,
        tx_id: vec![2; 32],
        block_hash: vec![0],
        tx: vec![0],
    };
    let address_tx_four = AddressTx {
        our_amount: 1,
        tx_id: vec![3; 32],
        block_hash: vec![0],
        tx: vec![0],
    };

    state.change_address(vec![0], txid_tx_hashmap(vec![address_tx_one.clone()]));
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);

    assert_eq!(state.addresses.len(), 1);
    assert_eq!(state.events.len(), 1);

    state.change_address(vec![0], txid_tx_hashmap(vec![address_tx_one.clone()]));
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.addresses.len(), 1);
    assert_eq!(state.events.len(), 1);

    state.change_address(
        vec![0],
        txid_tx_hashmap(vec![address_tx_one.clone(), address_tx_one.clone()]),
    );
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.addresses.len(), 1);
    assert_eq!(state.events.len(), 1);

    state.change_address(
        vec![0],
        txid_tx_hashmap(vec![address_tx_one.clone(), address_tx_two.clone()]),
    );
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.addresses.len(), 1);
    assert_eq!(state.events.len(), 2);

    state.change_address(
        vec![0],
        txid_tx_hashmap(vec![address_tx_one.clone(), address_tx_two.clone()]),
    );
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.addresses.len(), 1);
    assert_eq!(state.events.len(), 2);

    state.change_address(
        vec![0],
        txid_tx_hashmap(vec![address_tx_three.clone(), address_tx_four.clone()]),
    );
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.addresses.len(), 1);
    assert_eq!(state.events.len(), 4);

    let height_task = WatchHeight {
        id: 0,
        lifetime: 3,
        addendum: vec![],
    };
    state.watch_height(height_task, ServiceId::Syncer(Coin::Bitcoin));
    assert_eq!(state.lifetimes.len(), 2);
    assert_eq!(state.tasks_sources.len(), 2);
    assert_eq!(state.events.len(), 5);

    state.change_height(2, vec![0]);
    state.change_address(
        vec![0],
        txid_tx_hashmap(vec![address_tx_one.clone(), address_tx_two.clone()]),
    );
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.addresses.len(), 0);
    assert_eq!(state.events.len(), 6);
}

#[test]
fn syncer_state_height() {
    let mut state = SyncerState::new();
    let height_task = WatchHeight {
        id: 0,
        lifetime: 0,
        addendum: vec![],
    };
    let another_height_task = WatchHeight {
        id: 0,
        lifetime: 3,
        addendum: vec![],
    };

    state.watch_height(height_task, ServiceId::Syncer(Coin::Bitcoin));
    state.watch_height(another_height_task, ServiceId::Syncer(Coin::Bitcoin));
    assert_eq!(state.lifetimes.len(), 2);
    assert_eq!(state.tasks_sources.len(), 2);
    assert_eq!(state.watch_height.len(), 2);
    assert_eq!(state.events.len(), 2);

    state.change_height(1, vec![0]);
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.watch_height.len(), 1);
    assert_eq!(state.events.len(), 3);

    state.change_height(3, vec![0]);
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.watch_height.len(), 1);
    assert_eq!(state.events.len(), 4);

    state.change_height(3, vec![0]);
    assert_eq!(state.lifetimes.len(), 1);
    assert_eq!(state.tasks_sources.len(), 1);
    assert_eq!(state.watch_height.len(), 1);
    assert_eq!(state.events.len(), 4);

    state.change_height(4, vec![0]);
    assert_eq!(state.lifetimes.len(), 0);
    assert_eq!(state.tasks_sources.len(), 0);
    assert_eq!(state.watch_height.len(), 0);
    assert_eq!(state.events.len(), 4);
    return;
}
