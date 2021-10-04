use crate::farcaster_core::consensus::Decodable;
use crate::internet2::Duplex;
use crate::internet2::Encrypt;
use crate::internet2::TypedEnum;
use crate::rpc::request::SyncerdBridgeEvent;
use crate::rpc::Request;
use crate::syncerd::bitcoin_syncer::Synclet;
use crate::syncerd::runtime::SyncerServers;
use crate::syncerd::runtime::SyncerdTask;
use crate::syncerd::syncer_state::AddressTx;
use crate::syncerd::syncer_state::SyncerState;
use crate::syncerd::syncer_state::WatchedTransaction;
use crate::ServiceId;
use crate::{error::Error, syncerd::syncer_state::create_set};
use internet2::zmqsocket::Connection;
use internet2::zmqsocket::ZmqType;
use internet2::PlainTranscoder;
use internet2::ZMQ_CONTEXT;
use lnpbp::chain::Chain;
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

use crate::syncerd::*;
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

#[derive(Debug)]
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
                let mut block_hash: Option<Vec<u8>> = None;
                let mut confirmations: Option<u32> = Some(0);
                if let Some(tx_height) = tx.block_height {
                    if tx_height > 0 {
                        block_hash = Some(self.get_block_hash(tx_height).await);
                    }
                    println!(
                        "tx_height: {:?}, block_height: {:?}",
                        tx_height, block_height
                    );
                    confirmations = Some((block_height - tx_height + 1) as u32);
                }
                transactions.push(Transaction {
                    tx_id: hex::decode(tx.tx_hash.to_string()).unwrap(),
                    confirmations,
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
        network: monero::Network,
    ) -> Result<AddressNotif, Error> {
        let keypair = monero::ViewPair {
            spend: monero::PublicKey::from_slice(&address_addendum.spend_key.clone()).unwrap(),
            view: monero::PrivateKey::from_slice(&address_addendum.view_key.clone()).unwrap(),
        };
        let address = monero::Address::from_viewpair(network, &keypair);
        let wallet_client = monero_rpc::RpcClient::new(self.wallet_rpc_url.clone());
        let wallet = wallet_client.wallet();

        match wallet
            .open_wallet(address.to_string(), Some("pass".to_string()))
            .await
        {
            Err(err) => {
                trace!(
                    "error opening wallet: {:?}, falling back to generating a new wallet",
                    err
                );
                wallet
                    .generate_from_keys(GenerateFromKeysArgs {
                        restore_height: Some(address_addendum.from_height),
                        filename: address.to_string(),
                        address,
                        spendkey: None,
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

        wallet
            .refresh(Some(address_addendum.from_height))
            .await
            .unwrap();

        let mut category_selector: HashMap<GetTransfersCategory, bool> = HashMap::new();
        category_selector.insert(GetTransfersCategory::In, true);
        category_selector.insert(GetTransfersCategory::Out, true);
        category_selector.insert(GetTransfersCategory::Pending, true);
        category_selector.insert(GetTransfersCategory::Pool, true);

        let selector = GetTransfersSelector::<Range<u64>> {
            category_selector,
            subaddr_indices: None,
            account_index: None,
            filter_by_height: none!(),
        };

        let transfers = wallet.get_transfers(selector).await.unwrap();

        let mut address_txs: Vec<AddressTx> = vec![];
        for (_category, txs) in transfers.iter() {
            for tx in txs.iter() {
                error!("FIXME: tx set to vec![0]");
                address_txs.push(AddressTx {
                    our_amount: tx.amount,
                    tx_id: tx.txid.0.clone(),
                    tx: vec![0],
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
    fn run(
        &mut self,
        receive_task_channel: Receiver<SyncerdTask>,
        tx: zmq::Socket,
        syncer_address: Vec<u8>,
        syncer_servers: SyncerServers,
        chain: Chain,
        _polling: bool,
    ) {
        let _handle = std::thread::spawn(move || {
            let network = match chain {
                Chain::Mainnet | Chain::Regtest(_) => monero::Network::Mainnet,
                Chain::Testnet3 => monero::Network::Stagenet,
                Chain::Signet => monero::Network::Testnet,
                _ => {
                    error!("invalid chain type for monero: {}", chain);
                    return;
                }
            };
            let mut state = SyncerState::new();
            let mut rpc = MoneroRpc::new(
                syncer_servers.monero_daemon,
                syncer_servers.monero_rpc_wallet,
            );
            let mut connection = Connection::from_zmq_socket(ZmqType::Push, tx);
            let mut transcoder = PlainTranscoder {};
            let writer = connection.as_sender();

            let rt = Runtime::new().unwrap();
            rt.block_on(async {
                let block = rpc.check_block().await.unwrap();
                state.change_height(block.height, block.block_hash);
                info!("Entering monero_syncer event loop");
                loop {
                    match receive_task_channel.try_recv() {
                        Ok(syncerd_task) => {
                            match syncerd_task.task {
                                Task::Abort(task) => {
                                    state.abort(task.id, syncerd_task.source);
                                }
                                Task::BroadcastTransaction(task) => {
                                    error!("broadcast transaction not available for Monero");
                                    state.events.push((
                                        Event::TransactionBroadcasted(TransactionBroadcasted {
                                            id: task.id,
                                            tx: task.tx,
                                            error: Some("broadcast transaction not available for Monero".to_string()),
                                        }),
                                        syncerd_task.source,
                                    ));
                                }
                                Task::WatchAddress(task) => {
                                    match task.addendum.clone() {
                                        AddressAddendum::Monero(address_addendum) => {
                                            state.watch_address(task.clone(), syncerd_task.source).expect("Task::WatchAddress");
                                            let address_transactions =
                                                rpc.check_address(address_addendum, network).await.unwrap();
                                            state.change_address(
                                                task.addendum,
                                                create_set(address_transactions.txs),
                                            );
                                        }
                                        _ => {
                                            error!("Aborting watch address task - unable to decode address addendum");
                                            state.abort(task.id, syncerd_task.source);
                                        }
                                    }
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
                        let address_addendum = match watched_address.task.addendum.clone() {
                            AddressAddendum::Monero(address) => address,
                            _ => panic!("should never get an invalid address")
                        };
                        let address_transactions =
                            rpc.check_address(address_addendum.clone(), network).await.unwrap();
                        state.change_address(AddressAddendum::Monero(address_addendum), create_set(address_transactions.txs.clone()));
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
                    thread::sleep(std::time::Duration::from_secs(2));
                }
            });
        });
    }
}
