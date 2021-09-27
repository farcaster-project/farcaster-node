use crate::error::Error;
use crate::internet2::Duplex;
use crate::internet2::Encrypt;
use crate::internet2::TypedEnum;
use crate::rpc::request::SyncerdBridgeEvent;
use crate::rpc::Request;
use crate::syncerd::bitcoin_syncer::Synclet;
use crate::syncerd::runtime::SyncerdTask;
use crate::syncerd::syncer_state::create_set;
use crate::syncerd::syncer_state::AddressTx;
use crate::syncerd::syncer_state::SyncerState;
use crate::syncerd::types::{AddressAddendum, Task};
use crate::syncerd::Event;
use crate::syncerd::SyncerServers;
use crate::syncerd::TransactionBroadcasted;
use crate::syncerd::XmrAddressAddendum;
use internet2::zmqsocket::{Connection, ZmqType};
use internet2::PlainTranscoder;
use lnpbp::chain::Chain;
use monero::Hash;
use monero_rpc::GenerateFromKeysArgs;
use monero_rpc::GetBlockHeaderSelector;
use monero_rpc::GetTransfersCategory;
use monero_rpc::GetTransfersSelector;
use monero_rpc::TransferType;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::TryRecvError;
use std::sync::Arc;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::Receiver as TokioReceiver;
use tokio::sync::mpsc::Sender as TokioSender;
use tokio::sync::Mutex;

use hex;

#[derive(Debug, Clone)]
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
struct AddressNotif {
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
        let count: u64 = daemon
            .get_block_count()
            .await
            .expect("error getting height in monero syncer")
            .into();
        count - 1
    }

    async fn get_block_hash(&mut self, height: u64) -> Vec<u8> {
        let daemon_client = monero_rpc::RpcClient::new(self.node_rpc_url.clone());
        let daemon = daemon_client.daemon();
        let selector = GetBlockHeaderSelector::Height(height.into());
        let header = daemon
            .get_block_header(selector)
            .await
            .expect("error getting block hash in monero syncer");
        header.hash.0.to_vec()
    }

    async fn get_transactions(&mut self, tx_ids: Vec<Vec<u8>>) -> Result<Vec<Transaction>, Error> {
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
            .await?;

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
        Ok(transactions)
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
        let wallet = monero_rpc::RpcClient::new(self.wallet_rpc_url.clone()).wallet();
        let password = s!(" ");

        match wallet
            .open_wallet(address.to_string(), Some(password.clone()))
            .await
        {
            Err(err) => {
                error!(
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
                        password: password.clone(),
                        autosave_current: Some(true),
                    })
                    .await?;
                let res = wallet
                    .open_wallet(address.to_string(), Some(password))
                    .await?;
                debug!("Wallet successfully open {:?}", res)
            }
            Ok(res) => {
                debug!("Wallet successfully open {:?}", res)
            }
        }

        wallet.refresh(Some(address_addendum.from_height)).await?;

        let mut category_selector: HashMap<GetTransfersCategory, bool> = HashMap::new();
        category_selector.insert(GetTransfersCategory::In, true);
        category_selector.insert(GetTransfersCategory::Out, true);
        category_selector.insert(GetTransfersCategory::Pending, true);
        category_selector.insert(GetTransfersCategory::Pool, true);

        let selector = GetTransfersSelector {
            category_selector,
            subaddr_indices: None,
            account_index: None,
        };

        let mut transfers = wallet.get_transfers(selector).await?;

        let mut address_txs: Vec<AddressTx> = vec![];
        for (_category, mut txs) in transfers.drain() {
            for tx in txs.drain(..) {
                debug!("FIXME: tx set to vec![0]");
                address_txs.push(AddressTx {
                    our_amount: tx.amount,
                    tx_id: tx.txid.0,
                    tx: vec![0],
                });
            }
        }

        Ok(AddressNotif { txs: address_txs })
    }
}

pub struct MoneroSyncer {}

impl MoneroSyncer {
    pub fn new() -> Self {
        Self {}
    }
}

async fn run_syncerd_task_receiver(
    receive_task_channel: Receiver<SyncerdTask>,
    state: Arc<Mutex<SyncerState>>,
    tx_event: TokioSender<SyncerdBridgeEvent>,
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
                            error!("broadcast transaction not available for Monero");
                            tx_event.send(SyncerdBridgeEvent{
                                event: Event::TransactionBroadcasted(TransactionBroadcasted {
                                    id: task.id,
                                    tx: task.tx,
                                    error: Some("broadcast transaction not available for Monero".to_string()),
                                }),
                                source: syncerd_task.source,
                            }).await.expect("error sending the transaction broadcast event event from the syncer state");
                        }
                        Task::WatchAddress(task) => match task.addendum.clone() {
                            AddressAddendum::Monero(_) => {
                                let mut state_guard = state.lock().await;
                                state_guard
                                    .watch_address(task, syncerd_task.source)
                                    .expect("Monero Task::WatchAddress");
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
    syncer_servers: SyncerServers,
    network: monero::Network,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        let mut rpc = MoneroRpc::new(
            syncer_servers.monero_daemon,
            syncer_servers.monero_rpc_wallet,
        );
        loop {
            let state_guard = state.lock().await;
            let mut addresses = state_guard.addresses.clone();
            drop(state_guard);
            for (_, watched_address) in addresses.drain() {
                let address_addendum = match watched_address.task.addendum {
                    AddressAddendum::Monero(address) => address,
                    _ => panic!("should never get an invalid address"),
                };
                // we cannot parallelize polling here, since we have to open and close the
                // wallet
                let mut address_transactions = None;
                match rpc
                    .check_address(address_addendum.clone(), network.clone())
                    .await
                {
                    Ok(addr_txs) => {
                        address_transactions = Some(addr_txs);
                    }
                    Err(err) => {
                        error!("error polling addresses: {:?}", err);
                    }
                }
                if let Some(address_transactions) = address_transactions {
                    let mut state_guard = state.lock().await;
                    state_guard
                        .change_address(
                            AddressAddendum::Monero(address_addendum),
                            create_set(address_transactions.txs.clone()),
                        )
                        .await;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    })
}

fn height_polling(
    state: Arc<Mutex<SyncerState>>,
    syncer_servers: SyncerServers,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        let mut rpc = MoneroRpc::new(
            syncer_servers.monero_daemon,
            syncer_servers.monero_rpc_wallet,
        );
        loop {
            if let Some(block_notif) = rpc.check_block().await {
                let mut state_guard = state.lock().await;
                state_guard
                    .change_height(block_notif.height, block_notif.block_hash.into())
                    .await;
                let mut transactions = state_guard.transactions.clone();
                drop(state_guard);

                if transactions.len() > 0 {
                    let tx_ids: Vec<Vec<u8>> = transactions
                        .drain()
                        .map(|(_, tx)| tx.task.hash)
                        .collect();
                    let mut polled_transactions = vec![];
                    match rpc.get_transactions(tx_ids).await {
                        Ok(txs) => {
                            polled_transactions = txs;
                        }
                        Err(err) => {
                            error!("polling transactions error: {:?}", err);
                        }
                    }
                    let mut state_guard = state.lock().await;
                    for tx in polled_transactions.drain(..) {
                        state_guard
                            .change_transaction(tx.tx_id, tx.block_hash, tx.confirmations)
                            .await;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    })
}

fn unseen_transaction_polling(
    state: Arc<Mutex<SyncerState>>,
    syncer_servers: SyncerServers,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        let mut rpc = MoneroRpc::new(
            syncer_servers.monero_daemon,
            syncer_servers.monero_rpc_wallet,
        );
        loop {
            let state_guard = state.lock().await;
            let unseen_transactions = state_guard.unseen_transactions.clone();
            if unseen_transactions.len() > 0 {
                let transactions = state_guard.transactions.clone();
                let tx_ids: Vec<Vec<u8>> = unseen_transactions
                    .iter()
                    .map(|id| {
                        let tx = transactions.get(id).expect("attempted fetching a monero syncer state transaction that does not exist");
                        tx.task.hash.clone()
                    })
                    .collect();
                drop(state_guard);

                let mut polled_transactions = vec![];
                match rpc.get_transactions(tx_ids).await {
                    Ok(txs) => {
                        polled_transactions = txs;
                    }
                    Err(err) => {
                        error!("polling unseen transactions error: {:?}", err);
                    }
                }
                let mut state_guard = state.lock().await;
                for tx in polled_transactions.drain(..) {
                    state_guard
                        .change_transaction(tx.tx_id, tx.block_hash, tx.confirmations)
                        .await;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    })
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
                .unwrap();
        }
    });
}

impl Synclet for MoneroSyncer {
    fn run(
        &mut self,
        receive_task_channel: Receiver<SyncerdTask>,
        tx: zmq::Socket,
        syncer_address: Vec<u8>,
        syncer_servers: SyncerServers,
        chain: Chain,
        polling: bool,
    ) {
        if !polling {
            error!("monero syncer only supports polling for now - switching to polling=true");
        }
        let _handle = std::thread::spawn(move || {
            let rt = Runtime::new().unwrap();
            rt.block_on(async {
                let (event_tx, event_rx): (
                    TokioSender<SyncerdBridgeEvent>,
                    TokioReceiver<SyncerdBridgeEvent>,
                ) = tokio::sync::mpsc::channel(120);
                let state = Arc::new(Mutex::new(SyncerState::new(event_tx.clone())));

                run_syncerd_task_receiver(
                    receive_task_channel,
                    Arc::clone(&state),
                    event_tx.clone(),
                )
                .await;
                run_syncerd_bridge_event_sender(tx, event_rx, syncer_address).await;

                let network = match chain.clone() {
                    Chain::Mainnet | Chain::Regtest(_) => monero::Network::Mainnet,
                    Chain::Testnet3 => monero::Network::Stagenet,
                    Chain::Signet => monero::Network::Testnet,
                    _ => {
                        error!(
                            "invalid chain type for monero: {}- switching to mainnet",
                            chain
                        );
                        monero::Network::Mainnet
                    }
                };

                let address_handle =
                    address_polling(Arc::clone(&state), syncer_servers.clone(), network);

                // transaction polling is done in the same loop
                let height_handle = height_polling(Arc::clone(&state), syncer_servers.clone());

                let unseen_transaction_handle =
                    unseen_transaction_polling(Arc::clone(&state), syncer_servers.clone());

                let res =
                    tokio::try_join!(address_handle, height_handle, unseen_transaction_handle);
                debug!("exiting monero synclet run routine with: {:?}", res);
            });
        });
    }
}
