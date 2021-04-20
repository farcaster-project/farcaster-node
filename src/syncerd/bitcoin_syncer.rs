#![allow(dead_code, unused_must_use, path_statements, unreachable_code)]

use std::marker::{Sized, Send};
use std::convert::TryInto;
use std::io;
use std::collections::{HashSet, HashMap};

use async_trait::async_trait;
use hex;

use farcaster_core::tasks::*;
use farcaster_core::events::*;
use farcaster_core::interactions::syncer::Syncer;

use farcaster_chains::bitcoin::tasks::BtcAddressAddendum;

struct WatchedTransaction {
    hash: Vec<u8>,
    confirmation_bound: u16,
}

#[async_trait]
pub trait BitcoinRpc: Sized + Send {
    // getblockcount
    async fn get_height(&mut self) -> io::Result<u64>;

    // getblockhash
    async fn get_block_hash(&mut self, height: u64) -> io::Result<String>;

    // importaddress
    async fn import_address(&mut self, address: String) -> io::Result<()>;
    
    // listsinceblock
    // Must ensure valid schema for transactions/lastblock
    async fn list_since_block(&mut self, block: String, confirmations: u64, include_watch_only: bool) -> io::Result<serde_json::Value>;

    // sendrawtransaction
    async fn send_raw_transaction(&mut self, tx: Vec<u8>) -> io::Result<String>;
}

pub struct BitcoinSyncer<R: BitcoinRpc> {
    rpc: R,
    current_block: u64,
    current_block_hash: String,

    tasks: HashMap<i32, Task>,
    watch_height_count: u32,
    lifetimes: HashMap<u64, HashSet<i32>>,
    addresses: HashMap<i32, String>,
    transactions: HashMap<i32, WatchedTransaction>,
}

impl<R: BitcoinRpc> BitcoinSyncer<R> {
    fn add_lifetime(&mut self, lifetime: u64, id: i32) -> io::Result<()> {
        if lifetime < self.current_block {
            Err(io::Error::new(io::ErrorKind::Other, "Lifetime has already expired"))?;
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
}

#[async_trait]
impl<R: BitcoinRpc> Syncer for BitcoinSyncer<R> {
    async fn poll(&mut self) -> io::Result<Vec<Event>> {
        // If we're booting...
        if self.current_block == 0 {
            // Get the current block
            self.current_block = self.rpc.get_height().await?;
            self.current_block_hash = self.rpc.get_block_hash(self.current_block).await?;

            // Drop all tasks past their lifetime at boot
            let lifetimes: Vec<u64> = Iterator::collect(self.lifetimes.keys().map(|&x| x.to_owned()));
            for lifetime in lifetimes {
                if lifetime < self.current_block {
                    self.drop_lifetimes(&lifetime);
                }
            }
        }

        // Poll for new blocks
        let mut events: Vec<Event> = vec![];
        while self.current_block < self.rpc.get_height().await? {
            // Increment the current_block to match the new height
            self.current_block += 1;

            // Emit a height_changed event
            for _ in 0 .. self.watch_height_count {
                events.push(Event::HeightChanged(HeightChanged {
                    // Watch height count doesn't work out due to the unique IDs of each task
                    id: todo!(),
                    block: hex::decode(self.rpc.get_block_hash(self.current_block).await?).map_err(
                        |_| io::Error::new(io::ErrorKind::Other, "Bitcoin returned a non-hex block hash")
                    )?,
                    height: self.current_block
                }))
            }

            // Look for interactions with the addresses we're tracking
            // Usage of listsinceblock is optimal for this due to BTC indexing rules
            let transactions = self.rpc.list_since_block(self.current_block_hash, 6, true).await?;
            self.current_block_hash = transactions["lastblock"].as_str().expect("listsinceblock didn't have the lastblock field").to_owned();
            for tx in transactions["transactions"].as_array().expect("listsinceblock didn't have the transactions array") {
                todo!();
            }

            // Update transaction confirmations
            todo!();

            // Drop taks whose lifetime has expired
            self.drop_lifetimes(&self.current_block);
        }

        Ok(events)
    }

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
        // This is technically valid behavior; immediately pruning the task for being past its lifetime by never inserting it
        if !self.add_lifetime(task.lifetime, task.id).is_ok() {
            return;
        }
        self.tasks.insert(task.id, task.into());
        self.watch_height_count += 1;
    }

    async fn watch_address(&mut self, task: WatchAddress) -> io::Result<()> {
        let addendum = BtcAddressAddendum::deserialize(task.addendum)?;
        self.rpc.import_address(addendum.address);

        // This differs from the above and should potentially be changed to match
        // TODO: decide
        self.add_lifetime(task.lifetime, task.id)?;
        self.tasks.insert(task.id, task.into());

        todo!();

        Ok(())
    }

    async fn watch_transaction(&mut self, task: WatchTransaction) -> io::Result<()> {
        self.add_lifetime(task.lifetime, task.id)?;
        self.transactions.insert(
            task.id,
            WatchedTransaction{
                hash: task.hash.clone(),
                confirmation_bound: task.confirmation_bound,
            }
        );
        self.tasks.insert(task.id, task.into());

        Ok(())
    }

    async fn broadcast_transaction(&mut self, task: BroadcastTransaction) -> io::Result<()> {
        self.rpc.send_raw_transaction(task.tx).await?;
        Ok(())
    }
}
