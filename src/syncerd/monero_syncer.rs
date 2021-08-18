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
use monero_rpc::{GetTransfersCategory, GetTransfersSelector, TransferHeight};
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
    block_hash: Vec<u8>,
    ping_count: u8,
}

pub struct Block {
    height: u64,
    block_hash: Vec<u8>,
}

pub struct AddressNotif {
    address: XmrAddressAddendum,
    txs: Vec<AddressTx>,
}

pub struct Transaction {
    tx_id: Vec<u8>,
    confirmations: u64,
    block_hash: Vec<u8>,
}

impl MoneroRpc {
    fn new() -> Self {
        MoneroRpc {
            height: 0,
            block_hash: vec![0],
            ping_count: 0,
        }
    }

    async fn get_height(&mut self) -> u64 {
        let daemon_client =
            monero_rpc::RpcClient::new("http://node.monerooutreach.org:18081".to_string());
        let daemon = daemon_client.daemon();

        daemon.get_block_count().await.unwrap().into()
    }

    async fn get_transactions(&mut self, tx_ids: Vec<Vec<u8>>) -> Vec<Transaction> {
        let daemon_client =
            monero_rpc::RpcClient::new("http://node.monerooutreach.org:18081".to_string());
        let daemon = daemon_client.daemon_rpc();

        let mut buffer: [u8; 32] = [0; 32];
        let monero_txids = tx_ids
            .iter()
            .map(|tx_id| {
                hex::decode_to_slice(tx_id, &mut buffer).unwrap();
                Hash::from(buffer)
            })
            .collect();

        let txs = daemon
            .get_transactions(monero_txids, Some(true), Some(true))
            .await
            .unwrap();
        println!("{:?}", txs);

        let block_height = self.get_height().await;

        txs.txs
            .iter()
            .map(|tx| Transaction {
                tx_id: hex::decode(tx.tx_hash.clone()).unwrap(),
                confirmations: block_height - tx.block_height,
                block_hash: hex::decode(tx.tx_hash.clone()).unwrap(),
            })
            .collect()
    }

    async fn check_block(&mut self) -> Option<Block> {
        let mut block: Option<Block> = none!();
        let daemon_client =
            monero_rpc::RpcClient::new("http://node.monerooutreach.org:18081".to_string());
        let daemon = daemon_client.daemon();

        let height: u64 = daemon.get_block_count().await.unwrap().into();
        if height != self.height {
            let block_hash_arr: [u8; 32] = daemon.on_get_block_hash(height).await.unwrap().into();
            self.height = height;
            block = Some(Block {
                height,
                block_hash: Vec::from(block_hash_arr),
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
        let wallet_client = monero_rpc::RpcClient::new("http://127.0.0.1:18083".to_string());
        let wallet = wallet_client.wallet();

        match wallet
            .open_wallet(address.to_string(), Some("pass".to_string()))
            .await
        {
            Err(_) => {
                wallet
                    .generate_from_keys(
                        Some(2425400),
                        "test".to_string(),
                        address,
                        none!(),
                        keypair.view,
                        "pass".to_string(),
                        Some(true),
                    )
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
                    let daemon_client = monero_rpc::RpcClient::new(
                        "http://node.monerooutreach.org:18081".to_string(),
                    );
                    let daemon = daemon_client.daemon();
                    let selector = GetBlockHeaderSelector::Height(height.into());

                    let header = daemon.get_block_header(selector).await.unwrap();
                    block_hash = header.hash.0.to_vec();
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
            let mut rpc = MoneroRpc::new();
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

                        let tx_ids: Vec<Vec<u8>> = state
                            .transactions
                            .clone()
                            .iter()
                            .map(|(_, tx)| tx.task.hash.clone())
                            .collect();
                        let mut txs = rpc.get_transactions(tx_ids).await;
                        for tx in txs.drain(..) {
                            state.change_transaction(
                                tx.tx_id.to_vec(),
                                Some(tx.block_hash.clone()),
                                Some(tx.confirmations.try_into().unwrap()),
                            )
                        }
                    }
                    trace!("pending events: {:?}", state.events);

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

// #[test]
// pub fn syncer_state() {
//     let (tx, rx): (Sender<SyncerdTask>, Receiver<SyncerdTask>) = std::sync::mpsc::channel();
//     let tx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
//     let rx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
//     tx_event.connect("inproc://syncerdbridge").unwrap();
//     rx_event.bind("inproc://syncerdbridge").unwrap();

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
// }
