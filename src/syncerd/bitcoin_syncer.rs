// #![allow(dead_code, unused_must_use, path_statements, unreachable_code)]

use crate::farcaster_core::consensus::Decodable;
use crate::syncerd::syncer_state::AddressTx;
use crate::syncerd::syncer_state::SyncerState;
use crate::syncerd::syncer_state::WatchedTransaction;
use bitcoin::hashes::hex::FromHex;
use bitcoin::BlockHash;
use bitcoin::Script;
use electrum_client::raw_client::ElectrumSslStream;
use electrum_client::raw_client::RawClient;
use electrum_client::Hex32Bytes;
use electrum_client::{Client, ElectrumApi};
use farcaster_core::consensus::{self};
use farcaster_core::syncer::Error;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::io;
use std::iter::FromIterator;
use std::marker::{Send, Sized};
use std::sync::mpsc::Sender;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use hex;

use farcaster_core::chain::bitcoin::tasks::BtcAddressAddendum;
use farcaster_core::syncer::*;

pub trait Rpc {
    fn new() -> Self;

    fn get_height(&mut self) -> Result<u64, Error>;

    fn send_raw_transaction(&mut self, tx: Vec<u8>) -> Result<String, electrum_client::Error>;
}

pub struct ElectrumRpc {
    client: Client,
    height: u64,
    block_hash: BlockHash,
    addresses: HashMap<BtcAddressAddendum, Hex32Bytes>,
}

pub struct Block {
    height: u64,
    block_hash: BlockHash,
}

pub struct AddressNotif {
    address: BtcAddressAddendum,
    txs: Vec<AddressTx>,
}

impl ElectrumRpc {
    pub fn ping(&mut self) -> Result<(), electrum_client::Error> {
        Ok(self.client.ping()?)
    }

    pub fn subscribe_script(
        &mut self,
        address_addendum: BtcAddressAddendum,
    ) -> Result<AddressNotif, Error> {
        let data = self
            .client
            .script_subscribe(&bitcoin::Script::from(address_addendum.script_pubkey))
            .unwrap();
        self.addresses.insert(address_addendum, data.unwrap());
        let txs: Vec<AddressTx> = vec![];
        if let Some(digest) = data {
            self.handle_address_notification(address_addendum, digest, txs);
        }
        let notif = AddressNotif {
            address: address_addendum,
            txs,
        };
        Ok(notif)
    }

    pub fn new_block_check(&mut self) -> Option<Block> {
        let block: Option<Block> = none!();
        loop {
            let header = self.client.block_headers_pop();
            match header {
                Ok(Some(a)) => {
                    block = Some(Block {
                        height: a.height as u64,
                        block_hash: a.header.block_hash(),
                    });
                    println!("\n\n{:?}\n\n", self.height);
                }
                _ => break,
            }
        }
        block
    }

    // check if an address received a new transaction
    pub fn address_change_check(&mut self) -> Vec<AddressNotif> {
        let notifs: Vec<AddressNotif> = vec![];
        for (address, state) in self.addresses.iter() {
            let txs: Vec<AddressTx> = vec![];
            loop {
                // get pending notifications for this address/script_pubkey
                match self
                    .client
                    .script_pop(&bitcoin::Script::from(address.script_pubkey))
                {
                    Ok(Some(digest)) => {
                        if digest != *state {
                            self.handle_address_notification(*address, digest, txs)
                        }
                    }
                    // nothing left to process for this address - break
                    _ => break,
                }
            }
            if txs.len() > 0 {
                notifs.push(AddressNotif {
                    address: *address,
                    txs,
                })
            }
        }
        notifs
    }

    fn handle_address_notification(
        &mut self,
        address: BtcAddressAddendum,
        digest: Hex32Bytes,
        txs: Vec<AddressTx>,
    ) {
        self.addresses.remove_entry(&address);
        self.addresses.insert(address, digest);
        // now that we have established _something_ has changed get the full transaction history of the address
        let script = bitcoin::Script::from(address.script_pubkey);
        let tx_history = self.client.script_get_history(&script).unwrap();
        for hist in tx_history.iter() {
            let our_amount: u64 = 0;
            let txid = hist.tx_hash;
            // get the full transaction to calculate our_amount
            let tx = self.client.transaction_get(&txid).unwrap();
            for output in tx.output.iter() {
                if output.script_pubkey == script {
                    our_amount += output.value
                }
            }
            // if the transaction is mined, get the blockhash of the block containing it
            let block_hash = if hist.height > 0 {
                self.client
                    .transaction_get_verbose(&txid)
                    .unwrap()
                    .blockhash
                    .unwrap()
                    .to_vec()
            } else {
                vec![]
            };
            txs.push(AddressTx {
                our_amount,
                tx_id: txid.to_vec(),
                block_hash,
            })
        }
    }
}

impl Rpc for ElectrumRpc {
    fn new() -> Self {
        let client = Client::new("ssl://electrum.blockstream.info:50002").unwrap();
        let header = client.block_headers_subscribe().unwrap();
        println!("{:?}", header.height);

        Self {
            client,
            addresses: none!(),
            block_hash: header.header.block_hash(),
            height: header.height as u64,
        }
    }

    fn send_raw_transaction(&mut self, tx: Vec<u8>) -> Result<String, electrum_client::Error> {
        let result = self.client.transaction_broadcast_raw(&tx)?;
        Ok(result.to_string())
    }

    fn get_height(&mut self) -> Result<u64, Error> {
        Ok(self.height)
    }
}

#[test]
fn test_electrumrpc() {
    let mut rpc = ElectrumRpc::new();
    rpc.new_block_check();
}

pub trait Synclet {
    fn run(&mut self);
}

pub struct BitcoinSyncer {
    rpc: ElectrumRpc,
    current_block_hash: String,
    state: SyncerState,
    rx: Receiver<Task>,
}

impl BitcoinSyncer {
    pub fn new(rx: Receiver<Task>) -> Self {
        Self {
            rpc: ElectrumRpc::new(),
            current_block_hash: String::new(),
            state: SyncerState::new(),
            rx,
        }
    }
}

impl Synclet for BitcoinSyncer {
    fn run(&mut self) {
        let rx = self.rx;
        let _handle = std::thread::spawn(move || {
            let state = SyncerState::new();
            let rpc = ElectrumRpc::new();
            loop {
                match rx.try_recv() {
                    Ok(task) => {
                        match task {
                            Task::Abort(task) => {
                                state.abort(task).unwrap();
                            }
                            Task::BroadcastTransaction(task) => {
                                // TODO: match error and emit event with fail code
                                rpc.send_raw_transaction(task.tx);
                            }
                            Task::WatchAddress(task) => {
                                let res = std::io::Cursor::new(task.addendum);
                                let address_addendum =
                                    BtcAddressAddendum::consensus_decode(&mut res).unwrap();
                                state.watch_address(task);
                                let address_transactions =
                                    rpc.subscribe_script(address_addendum).unwrap();
                                state.change_address(task.addendum, address_transactions.txs);
                            }
                            Task::WatchHeight(task) => {
                                state.watch_height(task);
                            }
                            Task::WatchTransaction(task) => {
                                state.watch_transaction(task);
                            }
                        }
                        // added data to state, check if we received more from the channel
                        continue;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                    Err(TryRecvError::Empty) => {
                        // do nothing
                    }
                }

                // check if the server can still be reached
                rpc.ping();
                // check and process address/script_pubkey notifications
                let notifs = rpc.address_change_check();
                for address_transactions in notifs.iter() {
                    let serialized_address = consensus::serialize(&address_transactions.address);
                    state.change_address(serialized_address, address_transactions.txs);
                }
                // check and process new block notifications
                let new_block = rpc.new_block_check();
                if let Some(block_notif) = new_block {
                    state.change_height(block_notif.height, block_notif.block_hash.to_vec());

                    for (_, watched_tx) in state.transactions {
                        let tx_id: bitcoin::Txid =
                            bitcoin::Txid::from_hex(&hex::encode(watched_tx.hash)).unwrap();
                        let tx = rpc.client.transaction_get_verbose(&tx_id).unwrap();

                        let blockhash = match tx.blockhash {
                            Some(bh) => Some(bh.to_vec()),
                            None => none!(),
                        };

                        state.change_transaction(tx.txid.to_vec(), blockhash, tx.confirmations)
                    }
                }

                thread::sleep(std::time::Duration::from_secs(1));
            }
        });
    }
}

#[test]
pub fn syncer_state() {
    let (tx, rx): (Sender<Task>, Receiver<Task>) = std::sync::mpsc::channel();
    let syncer = BitcoinSyncer::new(rx);
    syncer.run();
    let task = Task::WatchHeight(WatchHeight {
        id: 0,
        lifetime: 0,
        addendum: vec![],
    });
    tx.send(task);
    thread::sleep(std::time::Duration::from_secs(100000000));
}
