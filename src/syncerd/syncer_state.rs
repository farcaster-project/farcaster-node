// #![allow(dead_code, unused_must_use, path_statements, unreachable_code)]

use crate::farcaster_core::consensus::Decodable;
use crate::ServiceId;
use microservices::rpc_connection::Request;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::io;
use std::marker::{Send, Sized};

use crate::serde::Deserialize;
use hex;

use farcaster_core::chain::bitcoin::tasks::BtcAddressAddendum;
use farcaster_core::syncer::*;

pub struct WatchedTransaction {
    pub hash: Vec<u8>,
    pub confirmation_bound: u16,
}

pub struct SyncerState {
    block_height: u64,
    block_hash: Vec<u8>,
    tasks: HashMap<u32, Task>,
    watch_height: HashMap<u32, WatchHeight>,
    lifetimes: HashMap<u64, HashSet<u32>>,
    pub addresses: HashMap<u32, WatchAddress>,
    pub transactions: HashMap<u32, WatchTransaction>,
    events: Vec<Event>,
    task_count: u32,
}

#[derive(Clone)]
pub struct AddressTx {
    pub our_amount: u64,
    pub tx_id: Vec<u8>,
    pub block_hash: Vec<u8>,
}

impl SyncerState {
    pub fn new() -> Self {
        Self {
            block_height: 0,
            block_hash: vec![0],
            tasks: HashMap::new(),
            watch_height: HashMap::new(),
            lifetimes: HashMap::new(),
            addresses: HashMap::new(),
            transactions: HashMap::new(),
            events: vec![],
            task_count: 0,
        }
    }

    pub fn abort(&mut self, task: Abort) -> Result<(), Error> {
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
        self.events.push(Event::TaskAborted(TaskAborted {
            id: task.id,
            success_abort: status,
        }));
        Ok(())
    }

    pub fn watch_height(&mut self, task: WatchHeight) -> Result<(), Error> {
        // increment the count to use it as a unique internal id
        self.task_count += 1;
        // This is technically valid behavior; immediately prune the task for being past its lifetime by never inserting it
        if !self.add_lifetime(task.lifetime, self.task_count).is_ok() {
            return Ok(());
        }
        self.watch_height.insert(self.task_count, task.clone());
        self.events.push(Event::HeightChanged(HeightChanged {
            id: task.id,
            block: self.block_hash.clone(),
            height: self.block_height,
        }));
        Ok(())
    }

    pub fn watch_address(&mut self, task: WatchAddress) -> Result<(), Error> {
        // increment the count to use it as a unique internal id
        self.task_count += 1;
        if self.add_lifetime(task.lifetime, self.task_count).is_err() {
            return Ok(());
        }
        self.addresses.insert(self.task_count, task);
        Ok(())
    }

    pub fn watch_transaction(&mut self, task: WatchTransaction) -> Result<(), Error> {
        // increment the count to use it as a unique internal id
        self.task_count += 1;

        let _tx = WatchedTransaction {
            hash: task.hash.clone(),
            confirmation_bound: task.confirmation_bound,
        };
        if self.add_lifetime(task.lifetime, self.task_count).is_err() {
            return Ok(());
        };
        self.transactions.insert(self.task_count, task);

        Ok(())
    }

    pub fn change_height(&mut self, height: u64, block: Vec<u8>) {
        self.drop_lifetimes();

        self.block_height = height;
        self.block_hash = block.clone();
        println!("height changed: {:?}", self.block_height);
        // Emit a height_changed event
        for task in self.watch_height.values() {
            self.events.push(Event::HeightChanged(HeightChanged {
                id: task.id,
                block: block.clone(),
                height: self.block_height,
            }))
        }
    }

    pub fn change_address(&mut self, address_addendum: Vec<u8>, txs: Vec<AddressTx>) {
        self.drop_lifetimes();

        for (id, addr) in self.addresses.iter() {
            if address_addendum == addr.addendum {
                for tx in txs.iter() {
                    self.events
                        .push(Event::AddressTransaction(AddressTransaction {
                            id: addr.id,
                            hash: tx.tx_id.clone(),
                            amount: tx.our_amount,
                            block: tx.block_hash.clone(),
                        }))
                }
            }
        }
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
            None => hex::decode("0x0").unwrap(),
        };
        let confs = match confirmations {
            Some(confs) => confs as i32,
            // per RFC, no confirmation should be encoded as -1
            None => -1,
        };

        for (id, watched_tx) in self.transactions.clone().iter() {
            if watched_tx.hash == tx_id {
                self.events
                    .push(Event::TransactionConfirmations(TransactionConfirmations {
                        id: watched_tx.id,
                        block: block.clone(),
                        confirmations: confs,
                    }));
                // prune the task once it has reached its confirmation bound
                if confs >= watched_tx.confirmation_bound as i32 {
                    self.remove_transaction(watched_tx.lifetime, *id);
                }
            }
        }
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
