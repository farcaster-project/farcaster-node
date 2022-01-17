use crate::internet2::Encrypt;
use crate::internet2::TypedEnum;
use crate::rpc::request::SyncerdBridgeEvent;
use crate::rpc::Request;
use crate::syncerd::opts::{Coin, Opts};
use crate::syncerd::runtime::SyncerdTask;
use crate::syncerd::runtime::Synclet;
use crate::syncerd::syncer_state::AddressTx;
use crate::syncerd::syncer_state::SyncerState;
use crate::syncerd::syncer_state::WatchedTransaction;
use crate::syncerd::types::{AddressAddendum, Boolean, Task};
use crate::syncerd::BroadcastTransaction;
use crate::syncerd::BtcAddressAddendum;
use crate::syncerd::EstimateFee;
use crate::syncerd::Event;
use crate::syncerd::FeeEstimation;
use crate::syncerd::GetTx;
use crate::syncerd::TaskTarget;
use crate::syncerd::TransactionBroadcasted;
use crate::syncerd::TransactionRetrieved;
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
use farcaster_core::blockchain::Network;
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

const RETRY_TIMEOUT: u64 = 5;
const PING_WAIT: u8 = 2;

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
            .map(|addr| addr.script_pubkey.clone())
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
    fn new(electrum_server: &str, polling: bool) -> Result<Self, electrum_client::Error> {
        let client = Client::new(electrum_server)?;
        let header = client.block_headers_subscribe()?;
        debug!("New ElectrumRpc at height {:?}", header.height);

        Ok(Self {
            client,
            addresses: none!(),
            height: header.height as u64,
            block_hash: header.header.block_hash(),
            ping_count: 0,
            polling,
        })
    }

    fn ping(&mut self) -> Result<(), Error> {
        if self.ping_count % PING_WAIT == 0 {
            self.client.ping()?;
            self.ping_count = 0;
        }
        self.ping_count += 1;
        Ok(())
    }

    pub fn script_subscribe(
        &mut self,
        address_addendum: BtcAddressAddendum,
    ) -> Result<AddressNotif, Error> {
        let script_status = if self.polling {
            None
        } else {
            self.client
                .script_subscribe(&address_addendum.script_pubkey)?
        };
        if self
            .addresses
            .insert(address_addendum.clone(), script_status)
            .is_some()
        {
        } else {
            debug!(
                "registering address {:?} with script_status {:?}",
                &address_addendum, &script_status
            );
        }
        let txs = query_addr_history(
            &mut self.client,
            address_addendum.script_pubkey.clone(),
            address_addendum.from_height,
        )?;
        logging(&txs, &address_addendum);
        let notif = AddressNotif {
            address: address_addendum,
            txs,
        };
        Ok(notif)
    }

    pub fn new_block_check(&mut self) -> Result<Vec<Block>, Error> {
        let mut blocks = vec![];
        if self.polling {
            let HeaderNotification { height, header } = self.client.block_headers_subscribe()?;
            self.height = height as u64;
            self.block_hash = header.block_hash();
            trace!("new height received: {:?}", self.height);
            blocks.push(Block {
                height: self.height,
                block_hash: self.block_hash,
            });
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
        Ok(blocks)
    }

    /// check if a subscribed address received a new transaction
    pub fn address_change_check(&mut self) -> Vec<AddressNotif> {
        let mut notifs: Vec<AddressNotif> = vec![];
        if self.polling {
            let scripts = Vec::<Script>::from(&*self);
            for (script, (address, _)) in scripts.into_iter().zip(self.addresses.clone()) {
                if let Ok(txs) = query_addr_history(&mut self.client, script, address.from_height) {
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

                while let Ok(Some(script_status)) = self.client.script_pop(script_pubkey) {
                    if Some(script_status) != previous_status {
                        if self
                            .addresses
                            .insert(address.clone(), Some(script_status))
                            .is_some()
                        {
                            debug!(
                                "updated address {:?} with script_status {:?}",
                                &address, &script_status
                            );
                        } else {
                            debug!(
                                "registering address {:?} with script_status {:?}",
                                &address, &script_status
                            );
                        }
                        if let Ok(txs) = query_addr_history(
                            &mut self.client,
                            script_pubkey.clone(),
                            address.from_height,
                        ) {
                            debug!("creating AddressNotif");
                            logging(&txs, &address);
                            let new_notif = AddressNotif {
                                address: address.clone(),
                                txs,
                            };
                            debug!("creating address notifications");
                            notifs.push(new_notif);
                        }
                    } else {
                        debug!("state did not change for given address");
                    }
                }
            }
        }
        notifs
    }

    async fn query_transactions(&self, state: Arc<Mutex<SyncerState>>, unseen: bool) {
        let state_guard = state.lock().await;
        let txids: Vec<Vec<u8>> = if unseen {
            state_guard
                .unseen_transactions
                .iter()
                .map(|task_id| state_guard.transactions[task_id].task.hash.clone())
                .collect()
        } else {
            state_guard
                .transactions
                .iter()
                .map(|(_, watched_tx)| watched_tx.task.hash.clone())
                .collect()
        };
        drop(state_guard);
        for tx_id in txids.iter() {
            let tx_id = bitcoin::Txid::from_slice(tx_id).unwrap();
            // Get the full transaction
            match self.client.transaction_get(&tx_id) {
                Ok(tx) => {
                    debug!("Updated tx: {}", &tx_id);
                    // Look for history of the first output (maybe last is generally less likely
                    // to be used multiple times, so more efficient?!)
                    let history = match self.client.script_get_history(&tx.output[0].script_pubkey)
                    {
                        Ok(history) => history,
                        Err(err) => {
                            trace!(
                                "error getting script history, treating as not found: {}",
                                err
                            );
                            let mut state_guard = state.lock().await;
                            state_guard
                                .change_transaction(
                                    tx_id.to_vec(),
                                    None,
                                    Some(0),
                                    bitcoin::consensus::serialize(&tx),
                                )
                                .await;
                            drop(state_guard);
                            continue;
                        }
                    };

                    let entry = history.iter().find(|history_res| {
                        history_res.tx_hash == tx_id
                    }).expect("Should be found in the history if we successfully queried `transaction_get`");

                    let (conf_in_block, blockhash) = match entry.height {
                        // Transaction unconfirmed (0 or -1)
                        i32::MIN..=0 => (None, None),
                        // Transaction confirmed at this height
                        1.. => {
                            // SAFETY: safe cast as it strictly greater than 0
                            let confirm_height = entry.height as usize;
                            let block = match self.client.block_header(confirm_height) {
                                Ok(block) => block,
                                Err(err) => {
                                    debug!(
                                        "error getting block header, treating as not found: {}",
                                        err
                                    );
                                    let mut state_guard = state.lock().await;
                                    state_guard
                                        .change_transaction(
                                            tx_id.to_vec(),
                                            None,
                                            Some(0),
                                            bitcoin::consensus::serialize(&tx),
                                        )
                                        .await;
                                    drop(state_guard);
                                    continue;
                                }
                            };
                            let blockhash = Some(block.block_hash().to_vec());
                            // SAFETY: safe cast u64 from usize
                            (Some(confirm_height as u64), blockhash)
                        }
                    };

                    let current_block_height = match self.client.block_headers_subscribe() {
                        // SAFETY: safe cast u64 from usize
                        Ok(block) => block.height as u64,
                        Err(err) => {
                            debug!(
                                "error getting top block header, treating as not found: {}",
                                err
                            );
                            let mut state_guard = state.lock().await;
                            state_guard
                                .change_transaction(
                                    tx_id.to_vec(),
                                    None,
                                    Some(0),
                                    bitcoin::consensus::serialize(&tx),
                                )
                                .await;
                            drop(state_guard);
                            continue;
                        }
                    };
                    let confs = match conf_in_block {
                        // check against block reorgs
                        Some(conf_in_block) if current_block_height < conf_in_block => 0,
                        // SAFETY: confirmations should not overflow 32-bits
                        Some(conf_in_block) => (current_block_height - conf_in_block) as u32 + 1,
                        None => 0,
                    };
                    let mut state_guard = state.lock().await;
                    state_guard
                        .change_transaction(
                            tx.txid().to_vec(),
                            blockhash,
                            Some(confs),
                            bitcoin::consensus::serialize(&tx),
                        )
                        .await;
                    drop(state_guard);
                }
                Err(err) => {
                    trace!("error getting transaction, treating as not found: {}", err);
                    let mut state_guard = state.lock().await;
                    state_guard
                        .change_transaction(tx_id.to_vec(), None, None, vec![])
                        .await;
                    drop(state_guard);
                }
            }
        }
    }
}

fn query_addr_history(
    client: &mut Client,
    script: Script,
    min_block_height: u64,
) -> Result<Vec<AddressTx>, Error> {
    // now that we have established _something_ has changed get the full transaction
    // history of the address
    let tx_hist = client.script_get_history(&script)?;
    trace!("history: {:?}", tx_hist);

    let mut addr_txs = vec![];
    for hist in tx_hist {
        // skip the transasction if it is either in the mempool (0 and -1) or below a
        // certain block height
        if hist.height <= min_block_height.try_into().unwrap() && hist.height > 0 {
            continue;
        }
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
            trace!("ignoring outgoing transaction in handle address notification, continuing");
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
    receive_task_channel: Receiver<SyncerdTask>,
    state: Arc<Mutex<SyncerState>>,
    transaction_broadcast_tx: TokioSender<(BroadcastTransaction, ServiceId)>,
    transaction_get_tx: TokioSender<(GetTx, ServiceId)>,
    estimate_fee_tx: TokioSender<(EstimateFee, ServiceId)>,
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
                        Task::GetTx(task) => {
                            transaction_get_tx
                                .send((task, syncerd_task.source))
                                .await
                                .expect("failed on transaction_get sender");
                        }
                        Task::EstimateFee(task) => {
                            estimate_fee_tx
                                .send((task, syncerd_task.source))
                                .await
                                .expect("failed on estimate fee sender");
                        }
                        Task::SweepAddress(_) => {
                            error!("sweep address not implemented for bitcoin syncer");
                        }
                        Task::Abort(task) => {
                            let mut state_guard = state.lock().await;
                            let respond = match task.respond {
                                Boolean::True => true,
                                Boolean::False => false,
                            };
                            state_guard
                                .abort(task.task_target, syncerd_task.source, respond)
                                .await;
                            drop(state_guard);
                        }
                        Task::BroadcastTransaction(task) => {
                            debug!("trying to broadcast tx: {:?}", task.tx.to_hex());
                            transaction_broadcast_tx
                                .send((task, syncerd_task.source))
                                .await
                                .expect("failed on transaction_broadcast_tx sender");
                        }
                        Task::WatchAddress(task) => match task.addendum.clone() {
                            AddressAddendum::Bitcoin(_) => {
                                let mut state_guard = state.lock().await;
                                state_guard
                                    .watch_address(task.clone(), syncerd_task.source)
                                    .expect("Bitcoin Task::WatchAddress");
                                drop(state_guard);
                            }
                            _ => {
                                error!("Aborting watch address task - unable to decode address addendum");
                                let mut state_guard = state.lock().await;
                                state_guard
                                    .abort(TaskTarget::TaskId(task.id), syncerd_task.source, true)
                                    .await;
                                drop(state_guard);
                            }
                        },
                        Task::WatchHeight(task) => {
                            let mut state_guard = state.lock().await;
                            state_guard.watch_height(task, syncerd_task.source).await;
                            drop(state_guard);
                        }
                        Task::WatchTransaction(task) => {
                            let mut state_guard = state.lock().await;
                            state_guard.watch_transaction(task, syncerd_task.source);
                            drop(state_guard);
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => {
                    // do nothing
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
}

fn address_polling(
    state: Arc<Mutex<SyncerState>>,
    electrum_server: String,
    polling: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        loop {
            let mut rpc = match ElectrumRpc::new(&electrum_server, polling) {
                Ok(client) => client,
                Err(err) => {
                    error!(
                        "failed to spawn electrum rpc client ({}) t in address polling: {:?}",
                        &electrum_server, err
                    );
                    // wait a bit before retrying the connection
                    tokio::time::sleep(std::time::Duration::from_secs(RETRY_TIMEOUT)).await;
                    continue;
                }
            };

            loop {
                if let Err(err) = rpc.ping() {
                    error!("error ping electrum client in address polling: {:?}", err);
                    // break this loop and retry, since the electrum rpc client is probably
                    // broken
                    break;
                }
                let state_guard = state.lock().await;
                let addresses = state_guard.addresses.clone();
                drop(state_guard);
                for (_, address) in addresses.clone() {
                    if let AddressAddendum::Bitcoin(address_addendum) = address.task.addendum {
                        let mut tx_set = HashSet::new();
                        match rpc.script_subscribe(address_addendum.clone()) {
                            Ok(addr_notif) if addr_notif.txs.is_empty() => {}
                            Ok(addr_notif) => {
                                logging(&addr_notif.txs, &address_addendum);
                                tx_set = create_set(addr_notif.txs);
                            }
                            // do nothing if we are already subscribed
                            Err(Error::Syncer(SyncerError::Electrum(
                                electrum_client::Error::AlreadySubscribed(_),
                            ))) => {}
                            Err(e) => {
                                error!("error in bitcoin address polling: {}", e);
                                break;
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
                }
                let mut addrs_notifs = rpc.address_change_check();
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
        }
    })
}

fn height_polling(
    state: Arc<Mutex<SyncerState>>,
    electrum_server: String,
    polling: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        // outer loop ensures the polling restarts if there is an error
        loop {
            let mut rpc = match ElectrumRpc::new(&electrum_server, polling) {
                Ok(client) => client,
                Err(err) => {
                    error!(
                        "failed to spawn electrum rpc client ({}) in height polling: {:?}",
                        &electrum_server, err
                    );
                    // wait a bit before retrying the connection
                    tokio::time::sleep(std::time::Duration::from_secs(RETRY_TIMEOUT)).await;
                    continue;
                }
            };

            let mut state_guard = state.lock().await;
            state_guard
                .change_height(rpc.height, rpc.block_hash.to_vec())
                .await;
            drop(state_guard);
            // inner loop actually polls
            loop {
                if let Err(err) = rpc.ping() {
                    error!("error ping electrum client in height polling: {:?}", err);
                    // break this loop and retry, since the electrum rpc client is probably
                    // broken
                    break;
                }
                let mut blocks = match rpc.new_block_check() {
                    Ok(blks) => blks,
                    Err(err) => {
                        error!("error polling bitcoin block height: {:?}", err);
                        // break this loop and retry, since the electrum rpc client is probably
                        // broken
                        break;
                    }
                };
                let mut state_guard = state.lock().await;
                let mut block_change = false;
                for block_notif in blocks.drain(..) {
                    block_change = state_guard
                        .change_height(block_notif.height, block_notif.block_hash.to_vec())
                        .await;
                }
                drop(state_guard);

                // if the blocks changed, query transactions
                if block_change {
                    rpc.query_transactions(Arc::clone(&state), false).await;
                }

                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            // wait a bit before retrying the connection
            tokio::time::sleep(std::time::Duration::from_secs(RETRY_TIMEOUT)).await;
        }
    })
}

fn unseen_transaction_polling(
    state: Arc<Mutex<SyncerState>>,
    electrum_server: String,
    polling: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        // outer loop ensures the polling restarts if there is an error
        loop {
            let rpc = match ElectrumRpc::new(&electrum_server, polling) {
                Ok(client) => client,
                Err(err) => {
                    error!(
                        "failed to spawn electrum rpc client ({}) in transaction polling: {:?}",
                        &electrum_server, err
                    );
                    // wait a bit before retrying the connection
                    tokio::time::sleep(std::time::Duration::from_secs(RETRY_TIMEOUT)).await;
                    continue;
                }
            };
            loop {
                rpc.query_transactions(Arc::clone(&state), true).await;
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        }
    })
}

#[derive(Default)]
pub struct BitcoinSyncer {}

impl BitcoinSyncer {
    pub fn new() -> Self {
        Self {}
    }
}

fn transaction_broadcasting(
    electrum_server: String,
    mut transaction_broadcast_rx: TokioReceiver<(BroadcastTransaction, ServiceId)>,
    tx_event: TokioSender<SyncerdBridgeEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        while let Some((broadcast_transaction, source)) = transaction_broadcast_rx.recv().await {
            match Client::new(&electrum_server).and_then(|broadcast_client| {
                broadcast_client.transaction_broadcast_raw(&broadcast_transaction.tx.clone())
            }) {
                Ok(txid) => {
                    tx_event
                        .send(SyncerdBridgeEvent {
                            event: Event::TransactionBroadcasted(TransactionBroadcasted {
                                id: broadcast_transaction.id,
                                tx: broadcast_transaction.tx,
                                error: None,
                            }),
                            source,
                        })
                        .await
                        .expect("error sending transaction broadcast event");
                    info!("Successfully broadcasted: {}", txid.bright_yellow_italic());
                }
                Err(e) => {
                    tx_event
                        .send(SyncerdBridgeEvent {
                            event: Event::TransactionBroadcasted(TransactionBroadcasted {
                                id: broadcast_transaction.id,
                                tx: broadcast_transaction.tx,
                                error: Some(format!("failed to broadcast tx: {}", e.err())),
                            }),
                            source,
                        })
                        .await
                        .expect("error sending transaction broadcast event");
                    error!("failed to broadcast tx: {}", e.err());
                }
            }
        }
    })
}

fn estimate_fee(
    electrum_server: String,
    mut estimate_fee_rx: TokioReceiver<(EstimateFee, ServiceId)>,
    tx_event: TokioSender<SyncerdBridgeEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        while let Some((estimate_fee_task, source)) = estimate_fee_rx.recv().await {
            match Client::new(&electrum_server).and_then(|estimate_fee_client| {
                estimate_fee_client.estimate_fee(estimate_fee_task.blocks_until_confirmation)
            }) {
                Ok(fee) => {
                    let fee = format!("{:.8} BTC", fee);
                    let amount = bitcoin::Amount::from_str_with_denomination(&fee).ok();
                    tx_event
                        .send(SyncerdBridgeEvent {
                            event: Event::FeeEstimation(FeeEstimation {
                                id: estimate_fee_task.id,
                                btc_per_kbyte: amount,
                            }),
                            source,
                        })
                        .await
                        .expect("error sending fee estimation event");
                    debug!("Successfully sent fee estimation");
                }
                Err(e) => {
                    println!("failed to retrieve fee estimation: {}", e.err());
                    tx_event
                        .send(SyncerdBridgeEvent {
                            event: Event::FeeEstimation(FeeEstimation {
                                id: estimate_fee_task.id,
                                btc_per_kbyte: None,
                            }),
                            source,
                        })
                        .await
                        .expect("error sending fee estimation event");
                }
            }
        }
    })
}

fn transaction_fetcher(
    electrum_server: String,
    mut transaction_get_rx: TokioReceiver<(GetTx, ServiceId)>,
    tx_event: TokioSender<SyncerdBridgeEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        while let Some((get_transaction, source)) = transaction_get_rx.recv().await {
            match Client::new(&electrum_server).and_then(|transaction_client| {
                transaction_client
                    .transaction_get(&bitcoin::Txid::from_slice(&get_transaction.hash).unwrap())
            }) {
                Ok(tx) => {
                    tx_event
                        .send(SyncerdBridgeEvent {
                            event: Event::TransactionRetrieved(TransactionRetrieved {
                                id: get_transaction.id,
                                tx: Some(tx),
                            }),
                            source,
                        })
                        .await
                        .expect("error sending transaction retrieved event");
                    info!(
                        "successfully retrieved tx: {:?}",
                        hex::encode(get_transaction.hash)
                    );
                }
                Err(e) => {
                    tx_event
                        .send(SyncerdBridgeEvent {
                            event: Event::TransactionRetrieved(TransactionRetrieved {
                                id: get_transaction.id,
                                tx: None,
                            }),
                            source,
                        })
                        .await
                        .expect("error sending transaction retrieved event");
                    info!("failed to retrieved tx: {:?}", e);
                }
            }
        }
    })
}

impl Synclet for BitcoinSyncer {
    fn run(
        &mut self,
        receive_task_channel: Receiver<SyncerdTask>,
        tx: zmq::Socket,
        syncer_address: Vec<u8>,
        opts: &Opts,
        _network: Network,
        polling: bool,
    ) -> Result<(), Error> {
        if let Some(electrum_server) = &opts.electrum_server {
            let electrum_server = electrum_server.clone();
            std::thread::spawn(move || {
                use tokio::runtime::Builder;
                let rt = Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async {
                    let (event_tx, event_rx): (
                        TokioSender<SyncerdBridgeEvent>,
                        TokioReceiver<SyncerdBridgeEvent>,
                    ) = tokio::sync::mpsc::channel(200);
                    let (transaction_broadcast_tx, transaction_broadcast_rx): (
                        TokioSender<(BroadcastTransaction, ServiceId)>,
                        TokioReceiver<(BroadcastTransaction, ServiceId)>,
                    ) = tokio::sync::mpsc::channel(200);
                    let (transaction_get_tx, transaction_get_rx): (
                        TokioSender<(GetTx, ServiceId)>,
                        TokioReceiver<(GetTx, ServiceId)>,
                    ) = tokio::sync::mpsc::channel(200);
                    let (estimate_fee_tx, estimate_fee_rx): (
                        TokioSender<(EstimateFee, ServiceId)>,
                        TokioReceiver<(EstimateFee, ServiceId)>,
                    ) = tokio::sync::mpsc::channel(200);
                    let state = Arc::new(Mutex::new(SyncerState::new(
                        event_tx.clone(),
                        Coin::Bitcoin,
                    )));

                    run_syncerd_task_receiver(
                        receive_task_channel,
                        Arc::clone(&state),
                        transaction_broadcast_tx,
                        transaction_get_tx,
                        estimate_fee_tx,
                    )
                    .await;
                    run_syncerd_bridge_event_sender(tx, event_rx, syncer_address).await;

                    let address_handle =
                        address_polling(Arc::clone(&state), electrum_server.clone(), polling);

                    let height_handle =
                        height_polling(Arc::clone(&state), electrum_server.clone(), polling);

                    let unseen_transaction_handle = unseen_transaction_polling(
                        Arc::clone(&state),
                        electrum_server.clone(),
                        polling,
                    );

                    let transaction_broadcast_handle = transaction_broadcasting(
                        electrum_server.clone(),
                        transaction_broadcast_rx,
                        event_tx.clone(),
                    );

                    let transaction_get_handle = transaction_fetcher(
                        electrum_server.clone(),
                        transaction_get_rx,
                        event_tx.clone(),
                    );

                    let estimate_fee_handle =
                        estimate_fee(electrum_server, estimate_fee_rx, event_tx.clone());

                    let res = tokio::try_join!(
                        address_handle,
                        height_handle,
                        unseen_transaction_handle,
                        transaction_broadcast_handle,
                        transaction_get_handle,
                        estimate_fee_handle,
                    );
                    debug!("exiting bitcoin synclet run routine with: {:?}", res);
                });
            });
            Ok(())
        } else {
            error!("Missing --electrum-server argument");
            Err(SyncerError::InvalidConfig.into())
        }
    }
}

fn logging(txs: &[AddressTx], address: &BtcAddressAddendum) {
    txs.iter().for_each(|tx| {
        trace!(
            "processing address {} notification txid {}",
            address.address.clone().unwrap(),
            bitcoin::Txid::from_slice(&tx.tx_id).unwrap().addr()
        );
    });
}
