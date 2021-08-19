use crate::error::Error;
use crate::farcaster_core::consensus::Decodable;
use crate::internet2::Duplex;
use crate::internet2::Encrypt;
use crate::internet2::TypedEnum;
use crate::rpc::request::SyncerdBridgeEvent;
use crate::rpc::Request;
use crate::syncerd::bitcoin_syncer::Synclet;
use crate::syncerd::runtime::SyncerdTask;
use crate::syncerd::syncer_state::AddressTx;
use crate::syncerd::syncer_state::SyncerState;
use crate::syncerd::syncer_state::WatchedTransaction;
use crate::ServiceId;
use bitcoin::hashes::hex::FromHex;
use bitcoin::Script;
use electrum_client::raw_client::ElectrumSslStream;
use electrum_client::raw_client::RawClient;
use electrum_client::Hex32Bytes;
use electrum_client::{Client, ElectrumApi};
use farcaster_core::consensus::{self};
use farcaster_core::monero::tasks::XmrAddressAddendum;
use internet2::zmqsocket::Connection;
use internet2::zmqsocket::ZmqType;
use internet2::PlainTranscoder;
use internet2::ZMQ_CONTEXT;
use monero::Hash;
use monero_rpc::BlockHash;
use monero_rpc::GetBlockHeaderSelector;
use monero_rpc::JsonTransaction;
use monero_rpc::{
    GenerateFromKeysArgs, GetTransfersCategory, GetTransfersSelector, TransferHeight,
};
use std::collections::{HashMap, HashSet};
use std::convert::TryInto;
use std::io;
use std::iter::FromIterator;
use std::marker::{Send, Sized};
use std::ops::Bound;
use std::ops::Range;
use std::sync::mpsc::Sender;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread;
use std::time::Duration;
use tokio::runtime::Runtime;

use hex;

use farcaster_core::syncer::*;
use std::str::FromStr;

trait Rpc {
    fn new() -> Self;

    fn get_height(&mut self) -> Result<u64, Error>;

    fn send_raw_transaction(&mut self, tx: Vec<u8>) -> Result<String, electrum_client::Error>;

    fn ping(&mut self) -> Result<(), Error>;
}

pub struct MoneroRpc {
    height: u64,
    node_rpc_url: String,
    wallet_rpc_url: String,
    block_hash: Vec<u8>,
}

pub struct Block {
    height: u64,
    block_hash: Vec<u8>,
}

pub struct AddressNotif {
    address: XmrAddressAddendum,
    txs: Vec<AddressTx>,
}

#[derive(Debug)]
pub struct Transaction {
    tx_id: Vec<u8>,
    confirmations: Option<u32>,
    block_hash: Option<Vec<u8>>,
}

impl MoneroRpc {
    fn new(node_rpc_url: String, wallet_rpc_url: String) -> Self {
        MoneroRpc {
            node_rpc_url,
            wallet_rpc_url,
            height: 0,
            block_hash: vec![0],
        }
    }

    async fn get_height(&mut self) -> u64 {
        let daemon_client = monero_rpc::RpcClient::new(self.node_rpc_url.clone());
        let daemon = daemon_client.daemon();
        let count: u64 = daemon.get_block_count().await.unwrap().into();
        count - 1
    }

    async fn get_block_hash(&mut self, height: u64) -> Vec<u8> {
        let daemon_client = monero_rpc::RpcClient::new(self.node_rpc_url.clone());
        let daemon = daemon_client.daemon();
        let selector = GetBlockHeaderSelector::Height(height.into());
        let header = daemon.get_block_header(selector).await.unwrap();
        header.hash.0.to_vec()
    }

    async fn get_transactions(&mut self, tx_ids: Vec<Vec<u8>>) -> Vec<Transaction> {
        let daemon_client = monero_rpc::RpcClient::new(self.node_rpc_url.clone());
        let daemon = daemon_client.daemon_rpc();

        let mut buffer: [u8; 32] = [0; 32];
        let monero_txids = tx_ids
            .iter()
            .map(|tx_id| {
                hex::decode_to_slice(hex::encode(tx_id), &mut buffer).unwrap();
                Hash::from(buffer)
            })
            .collect();

        let txs = daemon
            .get_transactions(monero_txids, Some(true), Some(true))
            .await
            .unwrap();

        let block_height = self.get_height().await;

        let mut transactions: Vec<Transaction> = vec![];
        if txs.txs.is_some() {
            for tx in txs.txs.unwrap().iter() {
                let mut block_hash = none!();
                if tx.block_height > 0 {
                    block_hash = Some(self.get_block_hash(tx.block_height).await);
                }
                transactions.push(Transaction {
                    tx_id: hex::decode(tx.tx_hash.to_string()).unwrap(),
                    confirmations: Some((block_height - tx.block_height) as u32),
                    block_hash,
                });
            }
        }
        if txs.missed_tx.is_some() {
            transactions.extend(txs.missed_tx.unwrap().iter().map(|tx| Transaction {
                tx_id: hex::decode(tx.to_string()).unwrap(),
                confirmations: None,
                block_hash: None,
            }));
        }
        println!("transactions: {:?}", transactions);
        transactions
    }

    async fn check_block(&mut self) -> Option<Block> {
        let mut block: Option<Block> = none!();
        let height = self.get_height().await;

        if height != self.height {
            let block_hash = self.get_block_hash(height).await;
            self.height = height;
            self.block_hash = block_hash.clone();
            block = Some(Block {
                height,
                block_hash: block_hash,
            });
        }
        block
    }

    async fn check_address(
        &mut self,
        address_addendum: XmrAddressAddendum,
    ) -> Result<AddressNotif, Error> {
        let network = monero::Network::default();
        let keypair = monero::ViewPair {
            view: monero::PrivateKey::from_slice(&address_addendum.view_key.clone()).unwrap(),
            spend: monero::PublicKey::from_slice(&address_addendum.spend_key.clone()).unwrap(),
        };
        let address = monero::Address::from_viewpair(network, &keypair);
        let wallet_client = monero_rpc::RpcClient::new(self.wallet_rpc_url.clone());
        let wallet = wallet_client.wallet();

        match wallet
            .open_wallet(address.to_string(), Some("pass".to_string()))
            .await
        {
            Err(_) => {
                wallet
                    .generate_from_keys(GenerateFromKeysArgs {
                        restore_height: Some(2425400),
                        filename: "test".to_string(),
                        address,
                        spendkey: none!(),
                        viewkey: keypair.view,
                        password: "pass".to_string(),
                        autosave_current: Some(true),
                    })
                    .await
                    .unwrap();
                wallet
                    .open_wallet(address.to_string(), Some("pass".to_string()))
                    .await
                    .unwrap();
            }
            _ => {}
        }

        let mut category_selector: HashMap<GetTransfersCategory, bool> = HashMap::new();
        category_selector.insert(GetTransfersCategory::In, true);
        category_selector.insert(GetTransfersCategory::Out, true);
        category_selector.insert(GetTransfersCategory::Pending, true);

        let selector = GetTransfersSelector::<Range<u64>> {
            category_selector,
            subaddr_indices: Some(vec![0]),
            account_index: Some(0),
            filter_by_height: none!(),
        };

        let transfers = wallet.get_transfers(selector).await.unwrap();

        let mut address_txs: Vec<AddressTx> = vec![];
        for (_category, txs) in transfers.iter() {
            for tx in txs.iter() {
                let mut block_hash = vec![];
                if let TransferHeight::Confirmed(height) = tx.height {
                    block_hash = self.get_block_hash(height.into()).await;
                }
                address_txs.push(AddressTx {
                    our_amount: tx.amount,
                    tx_id: tx.txid.0.clone(),
                    block_hash,
                });
            }
        }

        Ok(AddressNotif {
            address: address_addendum,
            txs: address_txs,
        })
    }
}

pub struct MoneroSyncer {}

impl MoneroSyncer {
    pub fn new() -> Self {
        Self {}
    }
}

impl Synclet for MoneroSyncer {
    fn run(&mut self, rx: Receiver<SyncerdTask>, tx: zmq::Socket, syncer_address: Vec<u8>) {
        let _handle = std::thread::spawn(move || {
            let mut state = SyncerState::new();
            let mut rpc = MoneroRpc::new(
                "http://node.monerooutreach.org:18081".into(),
                "http://127.0.0.1:18083".into(),
            );
            let mut connection = Connection::from_zmq_socket(ZmqType::Push, tx);
            let mut transcoder = PlainTranscoder {};
            let writer = connection.as_sender();

            let rt = Runtime::new().unwrap();
            rt.block_on(async {
                let block = rpc.check_block().await.unwrap();
                state.change_height(block.height, block.block_hash);

                loop {
                    match rx.try_recv() {
                        Ok(syncerd_task) => {
                            // rt.spawn(async {
                            match syncerd_task.task {
                                Task::Abort(task) => {
                                    state.abort(task, syncerd_task.source).unwrap();
                                }
                                Task::BroadcastTransaction(_task) => {
                                    error!("broadcast transaction not available for Monero");
                                }
                                Task::WatchAddress(task) => {
                                    let mut res = std::io::Cursor::new(task.addendum.clone());
                                    let address_addendum =
                                        XmrAddressAddendum::consensus_decode(&mut res).unwrap();
                                    state.watch_address(task.clone(), syncerd_task.source);
                                    let address_transactions =
                                        rpc.check_address(address_addendum).await.unwrap();
                                    state.change_address(task.addendum, address_transactions.txs);
                                }
                                Task::WatchHeight(task) => {
                                    state.watch_height(task, syncerd_task.source);
                                }
                                Task::WatchTransaction(task) => {
                                    state.watch_transaction(task, syncerd_task.source);
                                    let tx_ids: Vec<Vec<u8>> = state
                                        .transactions
                                        .clone()
                                        .iter()
                                        .map(|(_, tx)| tx.task.hash.clone())
                                        .collect();
                                    let mut txs = rpc.get_transactions(tx_ids).await;
                                    for tx in txs.drain(..) {
                                        state.change_transaction(
                                            tx.tx_id,
                                            tx.block_hash,
                                            tx.confirmations,
                                        );
                                    }
                                }
                            }
                            // added data to state, check if we received more from the channel before sending out events
                            continue;
                        }
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                        Err(TryRecvError::Empty) => {
                            // do nothing
                        }
                    }

                    // check and process address/script_pubkey notifications
                    for (_, watched_address) in state.addresses.clone().iter() {
                        let mut res = std::io::Cursor::new(watched_address.task.addendum.clone());
                        let address_addendum =
                            XmrAddressAddendum::consensus_decode(&mut res).unwrap();
                        let address_transactions =
                            rpc.check_address(address_addendum).await.unwrap();
                        let serialized_address =
                            consensus::serialize(&address_transactions.address);
                        state.change_address(serialized_address, address_transactions.txs.clone());
                    }

                    // check and process new block notifications
                    if let Some(block_notif) = rpc.check_block().await {
                        state.change_height(block_notif.height, block_notif.block_hash.into());

                        if state.transactions.len() > 0 {
                            let tx_ids: Vec<Vec<u8>> = state
                                .transactions
                                .clone()
                                .iter()
                                .map(|(_, tx)| tx.task.hash.clone())
                                .collect();
                            let mut txs = rpc.get_transactions(tx_ids).await;
                            for tx in txs.drain(..) {
                                state.change_transaction(tx.tx_id, tx.block_hash, tx.confirmations);
                            }
                        }
                    }
                    trace!("pending events: {:?}", state.events);
                    println!("pending events: {:?}", state.events);

                    // now consume the requests
                    for (event, source) in state.events.drain(..) {
                        let request =
                            Request::SyncerdBridgeEvent(SyncerdBridgeEvent { event, source });
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
                }
            });
        });
    }
}

// #[test]
// pub fn monero_syncer_state_transaction_event() {
//     let (tx, rx): (Sender<SyncerdTask>, Receiver<SyncerdTask>) = std::sync::mpsc::channel();
//     let tx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
//     let rx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
//     tx_event.connect("inproc://xmrsyncerdbridge").unwrap();
//     rx_event.bind("inproc://xmrsyncerdbridge").unwrap();

//     // let tx_id = "4c13a1ef5c0edb3c6000c65df031f216b12852b664c66ba42aa1489e37c7d7f1".to_string();
//     let tx_id = "aa63ffd3b8e4589d1273c63db49bc2d986f1dcb087e52ba116b9894fc22a9ea7".to_string();

//     let mut syncer = MoneroSyncer::new();
//     syncer.run(rx, tx_event, ServiceId::Syncer.into());
//     let task = SyncerdTask {
//         task: Task::WatchTransaction(WatchTransaction {
//             id: 0,
//             lifetime: 100000000,
//             hash: Vec::from_hex(&tx_id).unwrap(),
//             confirmation_bound: 65535,
//         }),
//         source: ServiceId::Syncer,
//     };
//     tx.send(task).unwrap();
//     let message = rx_event.recv_multipart(0);
//     assert!(message.is_ok());
//     println!("message received: {:?}", message);
//     let message = rx_event.recv_multipart(0);
//     assert!(message.is_ok());
//     println!("message received: {:?}", message);
//     let message = rx_event.recv_multipart(0);
//     assert!(message.is_ok());
//     println!("message received: {:?}", message);
// }

// #[test]
// pub fn monero_syncer_state_height_event() {
//     let (tx, rx): (Sender<SyncerdTask>, Receiver<SyncerdTask>) = std::sync::mpsc::channel();
//     let tx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
//     let rx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
//     tx_event.connect("inproc://monerosyncerdbridge").unwrap();
//     rx_event.bind("inproc://monerosyncerdbridge").unwrap();

//     let mut syncer = MoneroSyncer::new();
//     syncer.run(rx, tx_event, ServiceId::Syncer.into());
//     let task = SyncerdTask {
//         task: Task::WatchHeight(WatchHeight {
//             id: 0,
//             lifetime: 100000000,
//             addendum: vec![],
//         }),
//         source: ServiceId::Syncer,
//     };
//     tx.send(task).unwrap();
//     let message = rx_event.recv_multipart(0);
//     assert!(message.is_ok());
//     println!("message received: {:?}", message);
//     let message = rx_event.recv_multipart(0);
//     assert!(message.is_ok());
//     println!("message received: {:?}", message);
//     let message = rx_event.recv_multipart(0);
//     assert!(message.is_ok());
//     println!("message received: {:?}", message);
// }

// #[tokio::test]
// async fn monero_rpc_test() {
// let client = monero_rpc::RpcClient::new("http://127.0.0.1:18083".to_string());
// let wallet = client.wallet();
// let height = wallet.get_height().await;
// println!("{:?}", height);
// let key_images = wallet.export_key_images().await;
// println!("{:?}", key_images);
//
// let viewkey: monero::PrivateKey = monero::PrivateKey::from_str(
// "4ed383d30ed2872b5f8c8e2cb917be4a08e261ae5cb1a93f8c641c40a380880e",
// )
// .unwrap();
// let address = monero::Address::from_str("4BJHyDE7Sz4UebEJ8vM4rqYt2rtGqV7bX4DcCKDjs1BX9tmjAHKbwk8F3kHVi5k64nXF3bpuEHPQVHWb7TUNgE7B7ykocFy").unwrap();
// let password = "pass";
// let spendkey: Option<monero::PrivateKey> = none!();
//
// let wallet_create = wallet
// .generate_from_keys(
// Some(2425400),
// "test".to_string(),
// address,
// spendkey,
// viewkey,
// password.to_string(),
// Some(true),
// )
// .await;
// println!("{:?}", wallet_create);
//
// let wallet_open = wallet
// .open_wallet("test".to_string(), Some("pass".to_string()))
// .await;
// println!("{:?}", wallet_open);
// }
//
// #[tokio::test]
// async fn monero_daemon_test() {
// let daemon_client =
// monero_rpc::RpcClient::new("http://node.monerooutreach.org:18081".to_string());
// let daemon = daemon_client.daemon();
// let height = daemon.get_block_count().await;
// println!("{:?}", height);
// }
//
// #[tokio::test]
// async fn monero_daemon_transactions_test() {
// let tx_id = "7c50844eced8ab78a8f26a126fbc1f731134e0ae3e6f9ba0f205f98c1426ff60".to_string();
// let daemon_client =
// monero_rpc::RpcClient::new("http://node.monerooutreach.org:18081".to_string());
// let daemon = daemon_client.daemon_rpc();
// let mut fixed_hash: [u8; 32] = [0; 32];
// hex::decode_to_slice(tx_id, &mut fixed_hash).unwrap();
// let tx = daemon
// .get_transactions(vec![fixed_hash.into()], Some(true), Some(true))
// .await;
// println!("{:?}", tx);
// println!(
// "unlock time: {:?}",
// serde_json::from_str::<JsonTransaction>(&tx.unwrap().txs_as_json.unwrap()[0])
// );
// }
