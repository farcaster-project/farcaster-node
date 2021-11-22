use crate::error::{Error, SyncerError};
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
use crate::syncerd::types::{AddressAddendum, SweepAddressAddendum, Task};
use crate::syncerd::Event;
use crate::syncerd::SyncerServers;
use crate::syncerd::TaskTarget;
use crate::syncerd::TransactionBroadcasted;
use crate::syncerd::XmrAddressAddendum;
use internet2::zmqsocket::{Connection, ZmqType};
use internet2::PlainTranscoder;
use lnpbp::chain::Chain;
use monero::Hash;
use monero_rpc::{
    GenerateFromKeysArgs, GetBlockHeaderSelector, GetTransfersCategory, GetTransfersSelector,
    PrivateKeyType, TransferType,
};
use std::collections::HashMap;
use std::ops::Range;
use std::str::FromStr;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::Receiver as TokioReceiver;
use tokio::sync::mpsc::Sender as TokioSender;
use tokio::sync::Mutex;

use hex;

#[derive(Debug, Clone)]
pub struct MoneroRpc {
    height: u64,
    daemon: monero_rpc::DaemonClient,
    daemon_rpc: monero_rpc::DaemonRpcClient,
    block_hash: Vec<u8>,
}

#[derive(Debug)]
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
    fn new(node_rpc_url: String) -> Self {
        MoneroRpc {
            daemon: monero_rpc::RpcClient::new(node_rpc_url.clone()).daemon(),
            daemon_rpc: monero_rpc::RpcClient::new(node_rpc_url).daemon_rpc(),
            height: 0,
            block_hash: vec![0],
        }
    }

    async fn get_height(&mut self) -> Result<u64, Error> {
        let count: u64 = self.daemon.get_block_count().await?.into();
        Ok(count - 1)
    }

    async fn get_block_hash(&mut self, height: u64) -> Result<Vec<u8>, Error> {
        let selector = GetBlockHeaderSelector::Height(height);
        let header = self.daemon.get_block_header(selector).await?;
        Ok(header.hash.0.to_vec())
    }

    async fn get_transactions(&mut self, tx_ids: Vec<Vec<u8>>) -> Result<Vec<Transaction>, Error> {
        let mut buffer: [u8; 32] = [0; 32];
        let monero_txids = tx_ids
            .iter()
            .map(|tx_id| {
                hex::decode_to_slice(hex::encode(tx_id), &mut buffer).unwrap();
                Hash::from(buffer)
            })
            .collect();

        let txs = self
            .daemon_rpc
            .get_transactions(monero_txids, Some(true), Some(true))
            .await?;

        let block_height = self.get_height().await?;

        let mut transactions: Vec<Transaction> = vec![];
        if txs.txs.is_some() {
            for tx in txs.txs.unwrap().iter() {
                let mut block_hash: Option<Vec<u8>> = None;
                let mut confirmations: Option<u32> = Some(0);
                if let Some(tx_height) = tx.block_height {
                    if tx_height > 0 {
                        block_hash = Some(self.get_block_hash(tx_height).await?);
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

    async fn check_block(&mut self) -> Result<Block, Error> {
        let height = self.get_height().await?;

        if height != self.height {
            let block_hash = self.get_block_hash(height).await?;
            self.height = height;
            self.block_hash = block_hash.clone();
            Ok(Block { height, block_hash })
        } else {
            Err(Error::Syncer(SyncerError::NoIncrementToHeight))
        }
    }

    async fn check_address(
        &mut self,
        address_addendum: XmrAddressAddendum,
        network: monero::Network,
        wallet_mutex: Arc<Mutex<monero_rpc::WalletClient>>,
    ) -> Result<AddressNotif, Error> {
        let keypair = monero::ViewPair {
            spend: address_addendum.spend_key,
            view: address_addendum.view_key,
        };
        let address = monero::Address::from_viewpair(network, &keypair);
        let wallet_filename = format!("watch:{}", address);
        let password = s!(" ");

        let wallet = wallet_mutex.lock().await;
        trace!("taking check address lock");

        match wallet
            .open_wallet(wallet_filename.clone(), Some(password.clone()))
            .await
        {
            Err(err) => {
                warn!("wallet doesn't exist, generating a new wallet: {:?}", err);
                wallet
                    .generate_from_keys(GenerateFromKeysArgs {
                        restore_height: Some(address_addendum.from_height),
                        filename: wallet_filename.clone(),
                        address,
                        spendkey: None,
                        viewkey: keypair.view,
                        password: password.clone(),
                        autosave_current: Some(true),
                    })
                    .await?;
                wallet.open_wallet(wallet_filename, Some(password)).await?;
                debug!("Watch wallet opened successfully")
            }
            Ok(_) => {
                debug!("Watch wallet opened successfully")
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
            block_height_filter: Some(monero_rpc::BlockHeightFilter {
                min_height: Some(address_addendum.from_height),
                max_height: None,
            }),
        };

        let mut transfers = wallet.get_transfers(selector).await?;
        trace!("releasing check address lock");
        drop(wallet);

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

async fn sweep_address(
    dest_address: monero::Address,
    view: monero::PrivateKey,
    spend: monero::PrivateKey,
    network: &monero::Network,
    wallet_mutex: Arc<Mutex<monero_rpc::WalletClient>>,
) -> Result<Vec<Vec<u8>>, Error> {
    let keypair = monero::KeyPair { view, spend };
    let password = s!(" ");
    let address = monero::Address::from_keypair(*network, &keypair);
    let wallet_filename = format!("sweep:{}", address);

    let wallet = wallet_mutex.lock().await;
    trace!("taking sweep wallet lock");

    match wallet
        .open_wallet(wallet_filename.clone(), Some(password.clone()))
        .await
    {
        Ok(_) => {
            debug!("opened sweep wallet");
        }
        Err(err) => {
            warn!(
                "error opening to be sweeped wallet: {:?}, falling back to generating a new wallet",
                err,
            );
            wallet
                .generate_from_keys(GenerateFromKeysArgs {
                    restore_height: Some(1),
                    filename: wallet_filename.clone(),
                    address,
                    spendkey: Some(keypair.spend),
                    viewkey: keypair.view,
                    password: password.clone(),
                    autosave_current: Some(true),
                })
                .await?;
            wallet.open_wallet(wallet_filename, Some(password)).await?;
        }
    }

    wallet.refresh(Some(1)).await?;
    // failsafe to check if the wallet really supports spending
    wallet.query_key(PrivateKeyType::Spend).await?;
    let (account, addrs) = (0, None);
    let balance = wallet.get_balance(account, addrs).await?;
    // only sweep once all the balance is unlocked
    if balance.unlocked_balance != 0 {
        info!("sweeping address with balance: {:?}", balance);
        let sweep_args = monero_rpc::SweepAllArgs {
            address: dest_address,
            account_index: 0,
            subaddr_indices: None,
            priority: monero_rpc::TransferPriority::Default,
            mixin: 10,
            ring_size: 11,
            unlock_time: 0,
            get_tx_keys: None,
            below_amount: None,
            do_not_relay: None,
            get_tx_hex: None,
            get_tx_metadata: None,
        };
        let res = wallet.sweep_all(sweep_args).await?;
        let tx_ids: Vec<Vec<u8>> = res
            .tx_hash_list
            .iter()
            .filter_map(|hash| {
                let hash_str = hash.to_string();
                info!("sweep transaction hash {}", hash_str);
                hex::decode(hash_str).ok()
            })
            .collect();

        Ok(tx_ids)
    } else {
        info!(
            "retrying sweep, balance not unlocked yet. Unlocked balance {:?}. Total balance {:?}",
            balance.unlocked_balance, balance.balance
        );
        trace!("releasing sweep wallet lock");
        Ok(vec![])
    }
}

#[derive(Default)]
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
                        Task::SweepAddress(task) => match task.addendum.clone() {
                            SweepAddressAddendum::Monero(_) => {
                                info!("sweeping to address: {}", task);
                                let mut state_guard = state.lock().await;
                                state_guard.sweep_address(task, syncerd_task.source);
                            }
                            _ => {
                                error!("Aborting sweep address task - unable to decode sweep address addendum");
                                let mut state_guard = state.lock().await;
                                state_guard
                                    .abort(TaskTarget::TaskId(task.id), syncerd_task.source)
                                    .await;
                            }
                        },
                        Task::Abort(task) => {
                            let mut state_guard = state.lock().await;
                            state_guard
                                .abort(task.task_target, syncerd_task.source)
                                .await;
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
                                state_guard
                                    .abort(TaskTarget::TaskId(task.id), syncerd_task.source)
                                    .await;
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
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
}

fn address_polling(
    state: Arc<Mutex<SyncerState>>,
    syncer_servers: SyncerServers,
    network: monero::Network,
    wallet_mutex: Arc<Mutex<monero_rpc::WalletClient>>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        let mut rpc = MoneroRpc::new(syncer_servers.monero_daemon);
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
                let address_transactions = match rpc
                    .check_address(address_addendum.clone(), network, Arc::clone(&wallet_mutex))
                    .await
                {
                    Ok(address_transactions) => Some(address_transactions),
                    Err(err) => {
                        error!("error polling addresses: {:?}", err);
                        None
                    }
                };
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
        let mut rpc = MoneroRpc::new(syncer_servers.monero_daemon);
        loop {
            let block_notif = match rpc.check_block().await {
                Ok(notif) => Some(notif),
                Err(Error::Syncer(SyncerError::NoIncrementToHeight)) => None,
                Err(err) => {
                    error!("error processing height polling: {}", err);
                    None
                }
            };
            if let Some(block_notif) = block_notif {
                let mut state_guard = state.lock().await;
                state_guard
                    .change_height(block_notif.height, block_notif.block_hash)
                    .await;
                let mut transactions = state_guard.transactions.clone();
                drop(state_guard);

                if !transactions.is_empty() {
                    let tx_ids: Vec<Vec<u8>> =
                        transactions.drain().map(|(_, tx)| tx.task.hash).collect();
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

fn sweep_polling(
    state: Arc<Mutex<SyncerState>>,
    wallet: Arc<Mutex<monero_rpc::WalletClient>>,
    network: monero::Network,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        loop {
            let state_guard = state.lock().await;
            let sweep_addresses = state_guard.sweep_addresses.clone();
            drop(state_guard);
            for (id, sweep_address_task) in sweep_addresses.iter() {
                if let SweepAddressAddendum::Monero(addendum) = sweep_address_task.addendum.clone()
                {
                    let sweep_address_txs = sweep_address(
                        addendum.address,
                        addendum.view_key,
                        addendum.spend_key,
                        &network,
                        Arc::clone(&wallet),
                    )
                    .await
                    .unwrap_or_else(|err| {
                        warn!("error polling sweep address {:?}, retrying", err);
                        vec![]
                    });
                    if !sweep_address_txs.is_empty() {
                        let mut state_guard = state.lock().await;
                        state_guard.success_sweep(id, sweep_address_txs).await;
                        drop(state_guard);
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    })
}

fn unseen_transaction_polling(
    state: Arc<Mutex<SyncerState>>,
    syncer_servers: SyncerServers,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        let mut rpc = MoneroRpc::new(syncer_servers.monero_daemon);
        loop {
            let state_guard = state.lock().await;
            let unseen_transactions = state_guard.unseen_transactions.clone();
            if !unseen_transactions.is_empty() {
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
        let network = match chain {
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
        let _handle = std::thread::spawn(move || {
            use tokio::runtime::Builder;
            let rt = Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let wallet_mutex = Arc::new(Mutex::new(
                    monero_rpc::RpcClient::new(syncer_servers.monero_rpc_wallet.clone()).wallet(),
                ));
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

                let address_handle = address_polling(
                    Arc::clone(&state),
                    syncer_servers.clone(),
                    network,
                    Arc::clone(&wallet_mutex),
                );

                // transaction polling is done in the same loop
                let height_handle = height_polling(Arc::clone(&state), syncer_servers.clone());

                let unseen_transaction_handle =
                    unseen_transaction_polling(Arc::clone(&state), syncer_servers.clone());

                let sweep_handle =
                    sweep_polling(Arc::clone(&state), Arc::clone(&wallet_mutex), network);

                let res = tokio::try_join!(
                    address_handle,
                    height_handle,
                    unseen_transaction_handle,
                    sweep_handle
                );
                debug!("exiting monero synclet run routine with: {:?}", res);
            });
        });
    }
}
