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
use crate::syncerd::types::{AddressAddendum, Task};
use crate::syncerd::BtcAddressAddendum;
use crate::syncerd::Event;
use crate::syncerd::TransactionBroadcasted;
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
use std::ops::Range;
use std::sync::mpsc::Sender;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::Receiver as TokioReceiver;
use tokio::sync::mpsc::Sender as TokioSender;
use tokio::sync::Mutex;

use hex;

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

    fn ping(&mut self) {
        if self.ping_count % 10 == 0 {
            match self.client.ping() {
                Err(err) => {
                    error!("electrum ping failed with error: {:?}", err);
                }
                _ => {}
            }
            self.ping_count = 0;
        }
        self.ping_count += 1;
    }

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
        if self.polling {
            if let Ok(HeaderNotification { height, header }) = self.client.block_headers_subscribe()
            {
                self.height = height as u64;
                self.block_hash = header.block_hash();
                trace!("new height received: {:?}", self.height);
                blocks.push(Block {
                    height: self.height,
                    block_hash: self.block_hash,
                });
            }
        } else {
            while let Ok(Some(HeaderNotification { height, header })) =
                self.client.block_headers_pop()
            {
                self.height = height as u64;
                self.block_hash = header.block_hash();
                trace!("new height received: {:?}", self.height);
                blocks.push(Block {
                    height: self.height,
                    block_hash: self.block_hash,
                });
            }
        }
        blocks
    }

    /// check if a subscribed address received a new transaction
    pub fn address_change_check(&mut self) -> Vec<AddressNotif> {
        let mut notifs: Vec<AddressNotif> = vec![];
        if self.polling {
            let scripts = Vec::<Script>::from(&*self);
            for (script, (address, _)) in scripts.into_iter().zip(self.addresses.clone()) {
                if let Ok(txs) = query_addr_history(&mut self.client, script) {
                    logging(&txs, &address);
                    let new_notif = AddressNotif {
                        address: address.clone(),
                        txs,
                    };
                    notifs.push(new_notif);
                }
            }
        } else {
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
                        if let Ok(txs) = query_addr_history(&mut self.client, script_pubkey.clone())
                        {
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
        }
        notifs
    }

    async fn query_transactions(&self, state: Arc<Mutex<SyncerState>>) {
        let state_guard = state.lock().await;
        let transactions = state_guard.transactions.clone();
        drop(state_guard);
        for (_, watched_tx) in transactions.iter() {
            let tx_id = bitcoin::Txid::from_slice(&watched_tx.task.hash).unwrap();
            match self.client.transaction_get_verbose(&tx_id) {
                Ok(tx) => {
                    debug!("Updated tx: {}", &tx_id);
                    let blockhash = tx.blockhash.map(|x| x.to_vec());
                    let confs = match tx.confirmations {
                        Some(confs) => confs,
                        None => 0,
                    };
                    let mut state_guard = state.lock().await;
                    state_guard
                        .change_transaction(tx.txid.to_vec(), blockhash, Some(confs))
                        .await;
                    drop(state_guard);
                }
                Err(err) => {
                    debug!("error getting transaction, treating as not found: {}", err);
                    let mut state_guard = state.lock().await;
                    state_guard
                        .change_transaction(tx_id.to_vec(), None, None)
                        .await;
                    drop(state_guard);
                }
            }
        }
    }
}

fn query_addr_history(client: &mut Client, script: Script) -> Result<Vec<AddressTx>, Error> {
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

async fn run_syncerd_bridge_event_sender(
    tx: zmq::Socket,
    mut event_rx: TokioReceiver<SyncerdBridgeEvent>,
    syncer_address: Vec<u8>,
) {
    tokio::spawn(async move {
        let mut connection = Connection::from_zmq_socket(ZmqType::Push, tx);
        while let Some(event) = event_rx.recv().await {
            let mut transcoder = PlainTranscoder {};
            let writer = connection.as_sender();

            let request = Request::SyncerdBridgeEvent(event);
            trace!("sending request over syncerd bridge: {:?}", request);
            writer
                .send_routed(
                    &syncer_address,
                    &syncer_address,
                    &syncer_address,
                    &transcoder.encrypt(request.serialize()),
                )
                .expect("failed to send from bitcoin syncer to syncerd bridge");
        }
    });
}

async fn run_syncerd_task_receiver(
    electrum_server: String,
    receive_task_channel: Receiver<SyncerdTask>,
    state: Arc<Mutex<SyncerState>>,
    tx_event: TokioSender<SyncerdBridgeEvent>,
    electrum_client: Arc<Mutex<ElectrumRpc>>,
) {
    let task_receiver = Arc::new(Mutex::new(receive_task_channel));
    tokio::spawn(async move {
        loop {
            // this is a hack around the Receiver not being Sync
            let guard = task_receiver.lock().await;
            let syncerd_task = guard.try_recv();
            drop(guard);
            match syncerd_task {
                Ok(syncerd_task) => {
                    match syncerd_task.task {
                        Task::Abort(task) => {
                            let mut state_guard = state.lock().await;
                            state_guard.abort(task.id, syncerd_task.source).await;
                        }
                        Task::BroadcastTransaction(task) => {
                            // TODO: match error and emit event with fail code
                            trace!("trying to broadcast tx: {:?}", task.tx.to_hex());
                            let broadcast_client = Client::new(&electrum_server).unwrap();
                            match broadcast_client.transaction_broadcast_raw(&task.tx.clone()) {
                                Ok(txid) => {
                                    tx_event
                                        .send(SyncerdBridgeEvent {
                                            event: Event::TransactionBroadcasted(
                                                TransactionBroadcasted {
                                                    id: task.id,
                                                    tx: task.tx,
                                                    error: None,
                                                },
                                            ),
                                            source: syncerd_task.source,
                                        })
                                        .await
                                        .expect("error sending transaction broadcast event");
                                    info!("successfully broadcasted tx {}", txid.addr());
                                }
                                Err(e) => {
                                    tx_event
                                        .send(SyncerdBridgeEvent {
                                            event: Event::TransactionBroadcasted(
                                                TransactionBroadcasted {
                                                    id: task.id,
                                                    tx: task.tx,
                                                    error: Some(format!(
                                                        "failed to broadcast tx: {}",
                                                        e.err()
                                                    )),
                                                },
                                            ),
                                            source: syncerd_task.source,
                                        })
                                        .await
                                        .expect("error sending transactio broadcast event");
                                    error!("failed to broadcast tx: {}", e.err());
                                }
                            }
                        }
                        Task::WatchAddress(task) => match task.addendum.clone() {
                            AddressAddendum::Bitcoin(address_addendum) => {
                                let mut state_guard = state.lock().await;
                                state_guard
                                    .watch_address(task.clone(), syncerd_task.source)
                                    .expect("Bitcoin Task::WatchAddress");
                                drop(state_guard);
                                let mut electrum_client_guard = electrum_client.lock().await;
                                let mut tx_set = HashSet::new();
                                match electrum_client_guard
                                    .script_subscribe(address_addendum.clone())
                                {
                                    Ok(addr_notif) if addr_notif.txs.is_empty() => {
                                        let msg = "Address subscription successful, but no \
                                                 transactions on this BTC address yet, events \
                                                 will be emmited when transactions are observed";
                                        warn!("{}", &msg);
                                    }
                                    Ok(addr_notif) => {
                                        logging(&addr_notif.txs, &address_addendum);
                                        tx_set = create_set(addr_notif.txs);
                                    }
                                    Err(e) => {
                                        warn!("{}", e)
                                    }
                                }
                                let mut state_guard = state.lock().await;
                                state_guard
                                    .change_address(
                                        AddressAddendum::Bitcoin(address_addendum.clone()),
                                        tx_set,
                                    )
                                    .await;
                                drop(state_guard);
                            }
                            _ => {
                                error!("Aborting watch address task - unable to decode address addendum");
                                let mut state_guard = state.lock().await;
                                state_guard.abort(task.id, syncerd_task.source).await;
                            }
                        },
                        Task::WatchHeight(task) => {
                            let mut state_guard = state.lock().await;
                            state_guard.watch_height(task, syncerd_task.source).await;
                        }
                        Task::WatchTransaction(task) => {
                            let mut state_guard = state.lock().await;
                            state_guard.watch_transaction(task, syncerd_task.source);
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => {
                    // do nothing
                }
            }
        }
    });
}

fn address_polling(
    state: Arc<Mutex<SyncerState>>,
    rpc: Arc<Mutex<ElectrumRpc>>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        loop {
            let mut rpc_guard = rpc.lock().await;
            rpc_guard.ping();
            let mut addrs_notifs = rpc_guard.address_change_check();
            drop(rpc_guard);
            let mut state_guard = state.lock().await;
            while let Some(AddressNotif { address, txs }) = addrs_notifs.pop() {
                logging(&txs, &address);
                state_guard
                    .change_address(AddressAddendum::Bitcoin(address), create_set(txs))
                    .await;
            }
            drop(state_guard);
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    })
}

fn height_polling(
    state: Arc<Mutex<SyncerState>>,
    syncer_servers: SyncerServers,
    polling: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        let mut rpc = ElectrumRpc::new(syncer_servers.electrum_server, polling).unwrap();
        let mut state_guard = state.lock().await;
        state_guard
            .change_height(rpc.height, rpc.block_hash.to_vec())
            .await;
        drop(state_guard);
        rpc.client
            .block_headers_subscribe()
            .expect("failed subscribing to block headers in height polling");
        loop {
            rpc.ping();
            let mut state_guard = state.lock().await;
            for block_notif in rpc.new_block_check().iter() {
                state_guard
                    .change_height(block_notif.height, block_notif.block_hash.to_vec())
                    .await;
            }
            drop(state_guard);
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    })
}

fn transaction_polling(
    state: Arc<Mutex<SyncerState>>,
    syncer_servers: SyncerServers,
    polling: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        let rpc = ElectrumRpc::new(syncer_servers.electrum_server, polling).unwrap();
        loop {
            rpc.query_transactions(Arc::clone(&state)).await;
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    })
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
        receive_task_channel: Receiver<SyncerdTask>,
        tx: zmq::Socket,
        syncer_address: Vec<u8>,
        syncer_servers: SyncerServers,
        _chain: Chain,
        polling: bool,
    ) {
        std::thread::spawn(move || {
            let rt = Runtime::new().unwrap();
            rt.block_on(async {
                let (event_tx, event_rx): (
                    TokioSender<SyncerdBridgeEvent>,
                    TokioReceiver<SyncerdBridgeEvent>,
                ) = tokio::sync::mpsc::channel(120);
                let state = Arc::new(Mutex::new(SyncerState::new(event_tx.clone())));
                let electrum_client = Arc::new(Mutex::new(
                    ElectrumRpc::new(syncer_servers.electrum_server.clone(), polling).unwrap(),
                ));

                run_syncerd_task_receiver(
                    syncer_servers.electrum_server.clone(),
                    receive_task_channel,
                    Arc::clone(&state),
                    event_tx.clone(),
                    Arc::clone(&electrum_client),
                )
                .await;
                run_syncerd_bridge_event_sender(tx, event_rx, syncer_address).await;

                let address_handle =
                    address_polling(Arc::clone(&state), Arc::clone(&electrum_client));

                let height_handle =
                    height_polling(Arc::clone(&state), syncer_servers.clone(), polling);

                let transaction_handle =
                    transaction_polling(Arc::clone(&state), syncer_servers.clone(), polling);

                let res = tokio::try_join!(address_handle, height_handle, transaction_handle);
                debug!("exiting bitcoin synclet run routine with: {:?}", res);
            });
        });
    }
}

fn logging(txs: &Vec<AddressTx>, address: &BtcAddressAddendum) {
    txs.iter().for_each(|tx| {
        trace!(
            "processing address {} notification txid {}",
            address.address.clone().unwrap(),
            bitcoin::Txid::from_slice(&tx.tx_id).unwrap().addr()
        );
    });
}
