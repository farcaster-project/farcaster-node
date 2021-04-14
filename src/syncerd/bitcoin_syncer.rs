#![allow(dead_code, unused_must_use, path_statements)]

use std::marker::{Sized, Send};
use std::io;
use std::collections::{HashSet, HashMap};

use async_trait::async_trait;
use hex;

use farcaster_core::tasks::*;
use farcaster_core::interactions::syncer::Syncer;

struct WatchedTransaction {
    hash: Vec<u8>,
    confirmation_bound: u16,
}

#[async_trait]
pub trait BitcoinRpc: Sized + Send {
    // getblockcount
    async fn get_height(&mut self) -> u64;

    // sendrawtransaction
    async fn send_raw_transaction(&mut self, tx: Vec<u8>);
}

pub struct BitcoinSyncer<R: BitcoinRpc> {
    rpc: R,
    current_block: u64,

    tasks: HashMap<i32, Task>,
    watch_height_count: u32,
    lifetimes: HashMap<u64, HashSet<i32>>,
    addresses: HashMap<i32, String>,
    transactions: HashMap<i32, WatchedTransaction>,
}

impl<R: BitcoinRpc> BitcoinSyncer<R> {
    fn add_lifetime(&mut self, lifetime: u64, id: i32) -> bool {
        if lifetime < self.current_block {
          return false;
        }

        if let Some(lifetimes) = self.lifetimes.get_mut(&lifetime) {
            lifetimes.insert(id);
        } else {
            let mut lifetimes = HashSet::new();
            lifetimes.insert(id);
            self.lifetimes.insert(lifetime, lifetimes);
        }
        true
    }

    fn remove_lifetime(&mut self, lifetime: u64, id: i32) {
        if let Some(lifetimes) = self.lifetimes.get_mut(&lifetime) {
            lifetimes.remove(&id);
        }
    }

    fn drop_lifetimes(&mut self, lifetime: &u64) {
        for task in self.lifetimes.remove(lifetime).unwrap().iter() {
            self.tasks.remove(task);
            self.addresses.remove(task);
            self.transactions.remove(task);
        }
    }

    pub async fn poll(&mut self) {
        // If we're booting...
        if self.current_block == 0 {
            // Get the current block
            self.current_block = self.rpc.get_height().await;

            // Drop all tasks past their lifetime at boot
            let lifetimes: Vec<u64> = Iterator::collect(self.lifetimes.keys().map(|&x| x.to_owned()));
            for lifetime in lifetimes {
                if lifetime < self.current_block {
                    self.drop_lifetimes(&lifetime);
                }
            }
        }

        // Poll for new blocks
        while self.current_block < self.rpc.get_height().await {
            // Increment the current_block to match the new height
            self.current_block += 1;

            // Emit a height_changed event
            todo!();

            // Look for interactions with the addresses we're tracking
            // Usage of listsinceblock may be optimal for this due to BTC indexing rules
            todo!();

            // Update transaction confirmations
            todo!();

            // Drop taks whose lifetime has expired
            self.drop_lifetimes(&self.current_block);
        }
    }
}

#[async_trait]
impl<R: BitcoinRpc> Syncer for BitcoinSyncer<R> {
    async fn abort(&mut self, task: Abort) {
        let mut status = 0;
        match self.tasks.remove(&task.id) {
            None => {},
            Some(task) => {
                match task {
                    Task::Abort(_) => {},
                    Task::WatchHeight(height) => {
                        self.remove_lifetime(height.lifetime, height.id);
                        self.watch_height_count -= 1;
                    },
                    Task::WatchAddress(address) => {
                        self.remove_lifetime(address.lifetime, address.id);
                        self.addresses.remove(&address.id);
                    },
                    Task::WatchTransaction(tx) => {
                        self.remove_lifetime(tx.lifetime, tx.id);
                        self.transactions.remove(&tx.id);
                    },

                    // Just set an error code as we immediately attempt to broadcast a transaction when told to
                    Task::BroadcastTransaction(_) => {
                        status = 1;
                    },
                }
            }
        }

        // Emit the task_aborted event
        task.id;
        status;
        todo!()
    }

    async fn watch_height(&mut self, task: WatchHeight) {
        if !self.add_lifetime(task.lifetime, task.id) {
            return
        }
        self.tasks.insert(task.id, task.into());
        self.watch_height_count += 1;
    }

    async fn watch_address(&mut self, task: WatchAddress) {
        if !self.add_lifetime(task.lifetime, task.id) {
            return
        }
        self.tasks.insert(task.id, task.into());

        todo!()
    }

    async fn watch_transaction(&mut self, task: WatchTransaction) {
        if !self.add_lifetime(task.lifetime, task.id) {
            return
        }
        self.transactions.insert(
            task.id,
            WatchedTransaction{
                hash: task.hash.clone(),
                confirmation_bound: task.confirmation_bound,
            }
        );
        self.tasks.insert(task.id, task.into());
    }

    async fn broadcast_transaction(&mut self, task: BroadcastTransaction) {
        self.rpc.send_raw_transaction(task.tx).await;
    }
}
