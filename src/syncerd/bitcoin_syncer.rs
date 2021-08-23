// #![allow(dead_code, unused_must_use, path_statements, unreachable_code)]

use crate::error::Error;
use crate::farcaster_core::consensus::Decodable;
use crate::internet2::Duplex;
use crate::internet2::Encrypt;
use crate::internet2::TypedEnum;
use crate::rpc::request::SyncerdBridgeEvent;
use crate::rpc::Request;
use crate::syncerd::runtime::SyncerdTask;
use crate::syncerd::syncer_state::AddressTx;
use crate::syncerd::syncer_state::SyncerState;
use crate::syncerd::syncer_state::WatchedTransaction;
use crate::ServiceId;
use bitcoin::hashes::{hex::FromHex, Hash};
use bitcoin::BlockHash;
use bitcoin::Script;
use electrum_client::raw_client::ElectrumSslStream;
use electrum_client::raw_client::RawClient;
use electrum_client::Hex32Bytes;
use electrum_client::{Client, ElectrumApi};
use farcaster_core::consensus;
use internet2::zmqsocket::Connection;
use internet2::zmqsocket::ZmqType;
use internet2::PlainTranscoder;
use internet2::ZMQ_CONTEXT;
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::io;
use std::iter::FromIterator;
use std::marker::{Send, Sized};
use std::sync::mpsc::Sender;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread;
use std::time::Duration;
use tokio::runtime::Runtime;

use hex;

use farcaster_core::bitcoin::tasks::BtcAddressAddendum;
use farcaster_core::syncer::*;

pub trait Rpc {
    fn new() -> Self;

    fn get_height(&mut self) -> Result<u64, Error>;

    fn send_raw_transaction(&mut self, tx: Vec<u8>) -> Result<String, electrum_client::Error>;

    fn ping(&mut self) -> Result<(), Error>;
}

pub struct ElectrumRpc {
    client: Client,
    height: u64,
    block_hash: BlockHash,
    addresses: HashMap<BtcAddressAddendum, Hex32Bytes>,
    ping_count: u8,
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
    pub fn subscribe_script(
        &mut self,
        address_addendum: BtcAddressAddendum,
    ) -> Result<AddressNotif, Error> {
        let data = self
            .client
            .script_subscribe(&bitcoin::Script::from(
                address_addendum.script_pubkey.clone(),
            ))
            .unwrap();
        self.addresses
            .insert(address_addendum.clone(), data.unwrap());
        let mut txs: Vec<AddressTx> = vec![];
        if let Some(digest) = data {
            txs.extend(self.handle_address_notification(
                address_addendum.clone(),
                digest,
                txs.clone(),
            ));
        }
        let notif = AddressNotif {
            address: address_addendum,
            txs,
        };
        Ok(notif)
    }

    pub fn new_block_check(&mut self) -> Option<Block> {
        let mut block: Option<Block> = none!();
        loop {
            let header = self.client.block_headers_pop();
            match header {
                Ok(Some(a)) => {
                    block = Some(Block {
                        height: a.height as u64,
                        block_hash: a.header.block_hash(),
                    });
                    self.height = a.height as u64;
                    self.block_hash = a.header.block_hash();
                    trace!("new height received: {:?}", self.height);
                }
                _ => break,
            }
        }
        block
    }

    // check if an address received a new transaction
    pub fn address_change_check(&mut self) -> Vec<AddressNotif> {
        let mut notifs: Vec<AddressNotif> = vec![];
        for (address, state) in self.addresses.clone().iter() {
            let mut txs: Vec<AddressTx> = vec![];
            loop {
                // get pending notifications for this address/script_pubkey
                match self
                    .client
                    .script_pop(&bitcoin::Script::from(address.script_pubkey.clone()))
                {
                    Ok(Some(digest)) => {
                        if digest != *state {
                            txs.extend(self.handle_address_notification(
                                address.clone(),
                                digest,
                                txs.clone(),
                            ));
                        }
                    }
                    // nothing left to process for this address - break
                    _ => break,
                }
            }
            if txs.len() > 0 {
                notifs.push(AddressNotif {
                    address: address.clone(),
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
        mut txs: Vec<AddressTx>,
    ) -> Vec<AddressTx> {
        self.addresses.remove_entry(&address);
        self.addresses.insert(address.clone(), digest);
        // now that we have established _something_ has changed get the full transaction
        // history of the address
        let script = bitcoin::Script::from(address.script_pubkey);
        let tx_history = self.client.script_get_history(&script).unwrap();
        for hist in tx_history.iter() {
            let mut our_amount: u64 = 0;
            let txid = hist.tx_hash;
            // get the full transaction to calculate our_amount
            let tx = self
                .client
                .transaction_get(&txid)
                .expect("cant get transaction");
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
        txs
    }

    fn query_transactions(&self, state: &mut SyncerState) {
        for (_, watched_tx) in state.transactions.clone().iter() {
            let tx_id: bitcoin::Txid = bitcoin::Txid::from_slice(&watched_tx.task.hash).unwrap();
            let tx = self
                .client
                .transaction_get_verbose(&tx_id)
                .expect("transaction_ge_verbose");
            info!("Updated tx: {:?}", &tx);
            let blockhash = match tx.blockhash {
                Some(bh) => Some(bh.to_vec()),
                None => none!(),
            };

            state.change_transaction(tx.txid.to_vec(), blockhash, tx.confirmations)
        }
    }
}

impl Rpc for ElectrumRpc {
    fn new() -> Self {
        let client = Client::new("tcp://localhost:50001").unwrap();
        let header = client.block_headers_subscribe().unwrap();
        info!("New ElectrumRpc at height {:?}", header.height);

        Self {
            client,
            addresses: none!(),
            height: header.height as u64,
            block_hash: header.header.block_hash(),
            ping_count: 0,
        }
    }

    fn send_raw_transaction(&mut self, tx: Vec<u8>) -> Result<String, electrum_client::Error> {
        let result = self.client.transaction_broadcast_raw(&tx)?;
        Ok(result.to_string())
    }

    fn get_height(&mut self) -> Result<u64, Error> {
        Ok(self.height)
    }

    fn ping(&mut self) -> Result<(), Error> {
        if self.ping_count % 30 == 0 {
            if self.client.ping().is_err() {
                return Err(Error::Other("ping failed".to_string()));
            }
            self.ping_count = 0;
        }
        self.ping_count += 1;
        Ok(())
    }
}

#[test]
fn test_electrumrpc() {
    let mut rpc = ElectrumRpc::new();
    rpc.new_block_check();
}

pub trait Synclet {
    fn run(&mut self, rx: Receiver<SyncerdTask>, tx: zmq::Socket, syncer_address: Vec<u8>);
}

pub struct BitcoinSyncer {}

impl BitcoinSyncer {
    pub fn new() -> Self {
        Self {}
    }
}

impl Synclet for BitcoinSyncer {
    fn run(&mut self, rx: Receiver<SyncerdTask>, tx: zmq::Socket, syncer_address: Vec<u8>) {
        let _handle = std::thread::spawn(move || {
            let mut state = SyncerState::new();
            let mut rpc = ElectrumRpc::new();
            let mut connection = Connection::from_zmq_socket(ZmqType::Push, tx);
            let mut transcoder = PlainTranscoder {};
            let writer = connection.as_sender();

            state.change_height(rpc.height, rpc.block_hash.to_vec());
            info!("Entering bitcoin_syncer event loop");
            let mut i = 0;
            loop {
                // check if the server can still be reached
                rpc.ping().unwrap();

                match rx.try_recv() {
                    Ok(syncerd_task) => {
                        match syncerd_task.task {
                            Task::Abort(task) => {
                                state.abort(task.id, syncerd_task.source);
                            }
                            Task::BroadcastTransaction(task) => {
                                // TODO: match error and emit event with fail code
                                rpc.send_raw_transaction(task.tx).unwrap();
                            }
                            Task::WatchAddress(task) => {
                                let mut res = std::io::Cursor::new(task.addendum.clone());
                                match BtcAddressAddendum::consensus_decode(&mut res) {
                                    Err(_e) => {
                                        error!("Aborting watch address task - unable to decode address addendum");
                                        state.abort(task.id, syncerd_task.source);
                                    }
                                    Ok(address_addendum) => {
                                        state.watch_address(task.clone(), syncerd_task.source);
                                        let address_transactions =
                                            rpc.subscribe_script(address_addendum).unwrap();
                                        state.change_address(
                                            task.addendum,
                                            address_transactions.txs,
                                        );
                                    }
                                }
                            }
                            Task::WatchHeight(task) => {
                                state.watch_height(task, syncerd_task.source);
                            }
                            Task::WatchTransaction(task) => {
                                info!("Watch tx task: {}", task);
                                state.watch_transaction(task, syncerd_task.source);
                            }
                        }
                        // added data to state, check if we received more from the channel before
                        // sending out events
                        continue;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                    Err(TryRecvError::Empty) => {
                        // do nothing
                    }
                }

                // check and process address/script_pubkey notifications
                let notifs = rpc.address_change_check();
                for address_transactions in notifs.iter() {
                    let serialized_address = consensus::serialize(&address_transactions.address);
                    state.change_address(serialized_address, address_transactions.txs.clone());
                }
                // check and process new block notifications
                let new_block = rpc.new_block_check();
                if let Some(block_notif) = new_block {
                    state.change_height(block_notif.height, block_notif.block_hash.to_vec());
                }
                if !state.events.is_empty() || i == 0 || i % 25 == 0 {
                    trace!("pending events: {:?}\n emmiting them now", state.events);
                    rpc.query_transactions(&mut state);
                }
                // now consume the requests
                for (event, source) in state.events.drain(..) {
                    let request = Request::SyncerdBridgeEvent(SyncerdBridgeEvent { event, source });
                    trace!("sending request over syncerd bridge: {:?}", request);
                    writer
                        .send_routed(
                            &syncer_address,
                            &syncer_address,
                            &syncer_address,
                            &transcoder.encrypt(request.serialize()),
                        )
                        .unwrap();
                }

                thread::sleep(std::time::Duration::from_secs(1));
                i += 1;
            }
        });
    }
}

#[test]
pub fn syncer_state() {
    let (tx, rx): (Sender<SyncerdTask>, Receiver<SyncerdTask>) = std::sync::mpsc::channel();
    let tx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
    let rx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
    tx_event.connect("inproc://syncerdbridge").unwrap();
    rx_event.bind("inproc://syncerdbridge").unwrap();

    let mut syncer = BitcoinSyncer::new();
    syncer.run(rx, tx_event, ServiceId::Syncer.into());
    let task = SyncerdTask {
        task: Task::WatchHeight(WatchHeight {
            id: 0,
            lifetime: 100000000,
            addendum: vec![],
        }),
        source: ServiceId::Syncer,
    };
    tx.send(task).unwrap();
    let message = rx_event.recv_multipart(0);
    assert!(message.is_ok());
}
