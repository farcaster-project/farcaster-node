use crate::internet2::Encrypt;
use crate::internet2::TypedEnum;
use crate::rpc::request::SyncerdBridgeEvent;
use crate::rpc::Request;
use crate::syncerd::opts::Coin;
use crate::syncerd::runtime::SyncerServers;
use crate::syncerd::runtime::SyncerdTask;
use crate::syncerd::syncer_state::AddressTx;
use crate::syncerd::syncer_state::SyncerState;
use crate::syncerd::syncer_state::WatchedTransaction;
use crate::ServiceId;
use crate::{error::Error, syncerd::syncer_state::create_set};
use crate::{error::SyncerError, internet2::Duplex};
use crate::{farcaster_core::consensus::Decodable, LogStyle};
use bitcoin::hashes::{
    hex::{FromHex, ToHex},
    Hash,
};
use bitcoin::BlockHash;
use bitcoin::Script;
use electrum_client::Hex32Bytes;
use electrum_client::{raw_client::ElectrumSslStream, HeaderNotification};
use electrum_client::{raw_client::RawClient, GetHistoryRes};
use electrum_client::{Client, ElectrumApi};
use farcaster_core::consensus;
use internet2::zmqsocket::Connection;
use internet2::zmqsocket::ZmqType;
use internet2::PlainTranscoder;
use internet2::ZMQ_CONTEXT;
use lnpbp::chain::Chain;
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

use crate::syncerd::*;

pub trait Rpc: Sized {
    fn new(electrum_server: String, polling: bool) -> Result<Self, electrum_client::Error>;

    fn get_height(&mut self) -> u64;

    fn send_raw_transaction(&mut self, tx: Vec<u8>) -> Result<String, electrum_client::Error>;

    fn ping(&mut self) -> Result<(), Error>;
}

pub struct ElectrumRpc {
    client: Client,
    height: u64,
    block_hash: BlockHash,
    addresses: HashMap<BtcAddressAddendum, Option<Hex32Bytes>>,
    ping_count: u8,
    polling: bool,
}

impl From<&ElectrumRpc> for Vec<Script> {
    fn from(x: &ElectrumRpc) -> Self {
        x.addresses
            .keys()
            .filter_map(|addr| Some(addr.script_pubkey.clone()))
            .collect()
    }
}

#[derive(Debug)]
pub struct Block {
    height: u64,
    block_hash: BlockHash,
}

#[derive(Debug)]
pub struct AddressNotif {
    address: BtcAddressAddendum,
    txs: Vec<AddressTx>,
}

impl ElectrumRpc {
    pub fn script_subscribe(
        &mut self,
        address_addendum: BtcAddressAddendum,
    ) -> Result<AddressNotif, Error> {
        info!(
            "subscribing to script_pubkey {}",
            &address_addendum.script_pubkey
        );
        let script_status = if self.polling {
            None
        } else {
            self.client
                .script_subscribe(&address_addendum.script_pubkey)?
        };
        if let Some(_) = self
            .addresses
            .insert(address_addendum.clone(), script_status.clone())
        {
            info!(
                "updated address {:?} with script_status {:?}",
                &address_addendum, &script_status
            );
        } else {
            info!(
                "registering address {:?} with script_status {:?}",
                &address_addendum, &script_status
            );
        }
        let txs = query_addr_history(&mut self.client, address_addendum.script_pubkey.clone())?;
        logging(&txs, &address_addendum);
        let notif = AddressNotif {
            address: address_addendum,
            txs,
        };
        Ok(notif)
    }
    pub fn new_block_check(&mut self) -> Vec<Block> {
        let mut blocks = vec![];
        while let Ok(Some(HeaderNotification { height, header })) = self.client.block_headers_pop()
        {
            self.height = height as u64;
            self.block_hash = header.block_hash();
            trace!("new height received: {:?}", self.height);
            blocks.push(Block {
                height: self.height,
                block_hash: self.block_hash,
            });
        }
        blocks
    }

    pub fn address_polling(&mut self) -> Vec<AddressNotif> {
        let mut notifs = vec![];
        let scripts = Vec::<Script>::from(&*self);

        for (script, (address, script_status)) in scripts.into_iter().zip(self.addresses.clone()) {
            if let Ok(txs) = query_addr_history(&mut self.client, script) {
                logging(&txs, &address);
                let new_notif = AddressNotif {
                    address: address.clone(),
                    txs,
                };
                notifs.push(new_notif);
            }
        }
        notifs
    }

    /// check if a subscribed address received a new transaction
    pub fn address_change_check(&mut self) -> Vec<AddressNotif> {
        let mut notifs: Vec<AddressNotif> = vec![];
        for (address, previous_status) in self.addresses.clone().into_iter() {
            // get pending notifications for this address/script_pubkey
            let script_pubkey = &address.script_pubkey;

            while let Ok(Some(script_status)) = self.client.script_pop(&script_pubkey) {
                if Some(script_status) != previous_status {
                    if let Some(_) = self
                        .addresses
                        .insert(address.clone(), Some(script_status.clone()))
                    {
                        info!(
                            "updated address {:?} with script_status {:?}",
                            &address, &script_status
                        );
                    } else {
                        info!(
                            "registering address {:?} with script_status {:?}",
                            &address, &script_status
                        );
                    }
                    if let Ok(txs) = query_addr_history(&mut self.client, script_pubkey.clone()) {
                        info!("creating AddressNotif");
                        logging(&txs, &address);
                        let new_notif = AddressNotif {
                            address: address.clone(),
                            txs,
                        };
                        info!("creating address notifications");
                        notifs.push(new_notif);
                    }
                } else {
                    info!("state did not change for given address");
                }
            }
        }
        notifs
    }

    fn query_transactions(&self, state: &mut SyncerState) {
        for (_, watched_tx) in state.transactions.clone().iter() {
            let tx_id = bitcoin::Txid::from_slice(&watched_tx.task.hash).unwrap();
            match self.client.transaction_get_verbose(&tx_id) {
                Ok(tx) => {
                    debug!("Updated tx: {}", &tx_id);
                    let blockhash = tx.blockhash.map(|x| x.to_vec());
                    let confs = match tx.confirmations {
                        Some(confs) => confs,
                        None => 0,
                    };
                    state.change_transaction(tx.txid.to_vec(), blockhash, Some(confs))
                }
                Err(err) => {
                    debug!("error getting transaction, treating as not found: {}", err);
                    state.change_transaction(tx_id.to_vec(), None, None)
                }
            }
        }
    }
}

fn query_addr_history(client: &mut Client, script: Script) -> Result<Vec<AddressTx>, Error> {
    // if script_status.is_none() {
    //     Err(Error::Syncer(SyncerError::NoTxsOnAddress))?
    // }
    // now that we have established _something_ has changed get the full transaction
    // history of the address
    let tx_hist = client.script_get_history(&script)?;
    trace!("history: {:?}", tx_hist);
    let mut addr_txs = vec![];
    for hist in tx_hist {
        let mut our_amount: u64 = 0;
        let txid = hist.tx_hash;
        // get the full transaction to calculate our_amount
        let tx = client.transaction_get(&txid)?;
        let mut output_found = false;
        for output in tx.output.iter() {
            if output.script_pubkey == script {
                output_found = true;
                our_amount += output.value;
            }
        }
        if !output_found {
            debug!("ignoring outgoing transaction in handle address notification, continuing");
            continue;
        }
        addr_txs.push(AddressTx {
            our_amount,
            tx_id: txid.to_vec(),
            tx: bitcoin::consensus::serialize(&tx),
        })
    }
    Ok(addr_txs)
}

impl Rpc for ElectrumRpc {
    fn new(electrum_server: String, polling: bool) -> Result<Self, electrum_client::Error> {
        let client = Client::new(&electrum_server)?;
        let header = client.block_headers_subscribe()?;
        info!("New ElectrumRpc at height {:?}", header.height);

        Ok(Self {
            client,
            addresses: none!(),
            height: header.height as u64,
            block_hash: header.header.block_hash(),
            ping_count: 0,
            polling,
        })
    }

    fn send_raw_transaction(&mut self, tx: Vec<u8>) -> Result<String, electrum_client::Error> {
        let result = self.client.transaction_broadcast_raw(&tx)?;
        Ok(result.to_string())
    }

    fn get_height(&mut self) -> u64 {
        self.height
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

pub trait Synclet {
    fn run(
        &mut self,
        rx: Receiver<SyncerdTask>,
        tx: zmq::Socket,
        syncer_address: Vec<u8>,
        syncer_servers: SyncerServers,
        chain: Chain,
        polling: bool,
    );
}

pub struct BitcoinSyncer {}

impl BitcoinSyncer {
    pub fn new() -> Self {
        Self {}
    }
}

impl Synclet for BitcoinSyncer {
    fn run(
        &mut self,
        rx: Receiver<SyncerdTask>,
        tx: zmq::Socket,
        syncer_address: Vec<u8>,
        syncer_servers: SyncerServers,
        _chain: Chain,
        polling: bool,
    ) {
        let _handle = std::thread::spawn(move || {
            let mut state = SyncerState::new();
            let mut rpc = ElectrumRpc::new(syncer_servers.electrum_server, polling)
                .expect("Instantiating electrum client");
            let mut connection = Connection::from_zmq_socket(ZmqType::Push, tx);
            let mut transcoder = PlainTranscoder {};
            let writer = connection.as_sender();

            state.change_height(rpc.height, rpc.block_hash.to_vec());
            info!("Entering bitcoin_syncer event loop");
            let sleep_duration = std::time::Duration::from_secs(if rpc.polling { 5 } else { 1 });
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
                                trace!("trying to broadcast tx: {:?}", task.tx.to_hex());
                                match rpc.send_raw_transaction(task.tx.clone()) {
                                    Ok(txid) => {
                                        state.events.push((
                                            Event::TransactionBroadcasted(TransactionBroadcasted {
                                                id: task.id,
                                                tx: task.tx,
                                                error: None,
                                            }),
                                            syncerd_task.source,
                                        ));
                                        info!("successfully broadcasted tx {}", txid.addr());
                                    }
                                    Err(e) => {
                                        state.events.push((
                                            Event::TransactionBroadcasted(TransactionBroadcasted {
                                                id: task.id,
                                                tx: task.tx,
                                                error: Some(format!(
                                                    "failed to broadcast tx: {}",
                                                    e.err()
                                                )),
                                            }),
                                            syncerd_task.source,
                                        ));
                                        error!("failed to broadcast tx: {}", e.err());
                                    }
                                }
                            }
                            Task::WatchAddress(task) => match task.addendum.clone() {
                                AddressAddendum::Bitcoin(address_addendum) => {
                                    state
                                        .watch_address(task, syncerd_task.source)
                                        .expect("watch_address");
                                    match rpc.script_subscribe(address_addendum.clone()) {
                                        Ok(addr_notif) if addr_notif.txs.is_empty() => {
                                            let msg =
                                                    "Address subscription successful, but no \
                                                     transactions on this BTC address yet, events \
                                                     will be emmited when transactions are observed";
                                            warn!("{}", &msg);
                                        }
                                        Ok(addr_notif) => {
                                            logging(&addr_notif.txs, &address_addendum);
                                            state.change_address(
                                                AddressAddendum::Bitcoin(address_addendum.clone()),
                                                create_set(addr_notif.txs),
                                            );
                                        }
                                        Err(e) => {
                                            warn!("{}", e)
                                        }
                                    }
                                }
                                _ => {
                                    error!("Aborting watch address task - received invalid address addendum");
                                    state.abort(task.id, syncerd_task.source);
                                }
                            },
                            Task::WatchHeight(task) => {
                                state.watch_height(task, syncerd_task.source);
                            }
                            Task::WatchTransaction(task) => {
                                trace!("Watch tx task: {}", task);
                                state.watch_transaction(task, syncerd_task.source);
                            }
                        }
                        // added data to state, check if we received more from the channel before
                        // sending out events
                        continue;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        error!("disconnected");
                        return;
                    }
                    Err(TryRecvError::Empty) => {
                        // do nothing
                    }
                }

                // check and process address/script_pubkey notifications
                let mut addrs_notifs = if rpc.polling {
                    rpc.address_polling()
                } else {
                    rpc.address_change_check()
                };
                while let Some(AddressNotif { address, txs }) = addrs_notifs.pop() {
                    logging(&txs, &address);
                    state.change_address(AddressAddendum::Bitcoin(address), create_set(txs));
                }
                // check and process new block notifications
                let mut new_blocks = rpc.new_block_check();
                while let Some(block_notif) = new_blocks.pop() {
                    state.change_height(block_notif.height, block_notif.block_hash.to_vec());
                }
                if !state.events.is_empty() || i == 0 || i % 5 == 0 {
                    trace!("pending events: {:?}\n emmiting them now", state.events);
                    rpc.query_transactions(&mut state);
                }

                // now consume the requests
                while let Some((event, source)) = state.events.pop() {
                    debug!(
                        "sending event {} to {} over syncerd bridge",
                        &event, &source
                    );
                    let request = Request::SyncerdBridgeEvent(SyncerdBridgeEvent { event, source });
                    writer
                        .send_routed(
                            &syncer_address,
                            &syncer_address,
                            &syncer_address,
                            &transcoder.encrypt(request.serialize()),
                        )
                        .expect("Failed on send_routed from bitcoin_syncer");
                }

                thread::sleep(sleep_duration);
                i += 1;
            }
        });
    }
}

fn logging(txs: &Vec<AddressTx>, address: &BtcAddressAddendum) {
    txs.iter().for_each(|tx| {
        trace!(
            "processing address {} notification txid {}",
            address.address.clone().unwrap(),
            bitcoin::Txid::from_slice(&tx.tx_id).unwrap().addr()
        )
    });
}
