use crate::error::{Error, SyncerError};
use crate::internet2::DuplexConnection;
use crate::internet2::Encrypt;
use crate::internet2::TypedEnum;
use crate::rpc::request::SyncerdBridgeEvent;
use crate::rpc::Request;
use crate::service::LogStyle;
use crate::syncerd::opts::Opts;
use crate::syncerd::runtime::SyncerdTask;
use crate::syncerd::runtime::Synclet;
use crate::syncerd::syncer_state::create_set;
use crate::syncerd::syncer_state::AddressTx;
use crate::syncerd::syncer_state::SyncerState;
use crate::syncerd::types::{AddressAddendum, Boolean, SweepAddressAddendum, Task};
use crate::syncerd::Event;
use crate::syncerd::TaskTarget;
use crate::syncerd::TransactionBroadcasted;
use crate::syncerd::XmrAddressAddendum;
use farcaster_core::blockchain::{Blockchain, Network};
use internet2::zeromq::{Connection, ZmqSocketType};
use internet2::PlainTranscoder;
use monero::Hash;
use monero_rpc::{
    GenerateFromKeysArgs, GetBlockHeaderSelector, GetTransfersCategory, GetTransfersSelector,
    PrivateKeyType, TransferType,
};
use std::collections::HashMap;
use std::fs;
use std::ops::Range;
use std::path::PathBuf;
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
    daemon_json_rpc: monero_rpc::DaemonJsonRpcClient,
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
            daemon_json_rpc: monero_rpc::RpcClient::new(node_rpc_url.clone()).daemon(),
            daemon_rpc: monero_rpc::RpcClient::new(node_rpc_url).daemon_rpc(),
            height: 0,
            block_hash: vec![0],
        }
    }

    async fn get_height(&mut self) -> Result<u64, Error> {
        let count: u64 = self.daemon_json_rpc.get_block_count().await?.into();
        Ok(count - 1)
    }

    async fn get_block_hash(&mut self, height: u64) -> Result<Vec<u8>, Error> {
        let selector = GetBlockHeaderSelector::Height(height);
        let header = self.daemon_json_rpc.get_block_header(selector).await?;
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
        for tx in txs.txs.iter().flatten() {
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
        transactions.extend(txs.missed_tx.iter().flatten().map(|tx| Transaction {
            tx_id: hex::decode(tx.to_string()).unwrap(),
            confirmations: None,
            block_hash: None,
        }));
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

    async fn check_address_lws(
        &mut self,
        address_addendum: XmrAddressAddendum,
        network: monero::Network,
        monero_lws_url: String,
    ) -> Result<AddressNotif, Error> {
        let keypair = monero::ViewPair {
            spend: address_addendum.spend_key,
            view: address_addendum.view_key,
        };
        let address = monero::Address::from_viewpair(network, &keypair);
        let daemon_client = monero_lws::LwsRpcClient::new(monero_lws_url);
        trace!("checking txs through lws for address {}", address);
        let mut txs = daemon_client.get_address_txs(address, keypair.view).await?;
        trace!("received txs {:?} from lws for address {}", txs, address);
        let address_txs: Vec<AddressTx> = txs
            .transactions
            .drain(..)
            .map(|tx| AddressTx {
                our_amount: tx.total_received.parse::<u64>().unwrap(),
                tx_id: tx.hash.0.to_bytes().into(),
                tx: vec![],
            })
            .collect();
        Ok(AddressNotif { txs: address_txs })
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
                debug!("wallet doesn't exist, generating a new wallet: {:?}", err);
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
                    our_amount: tx.amount.as_pico(),
                    tx_id: tx.txid.0,
                    tx: vec![0],
                });
            }
        }

        Ok(AddressNotif { txs: address_txs })
    }
}

async fn sweep_address(
    destination_address: monero::Address,
    view: monero::PrivateKey,
    spend: monero::PrivateKey,
    minimum_balance: monero::Amount,
    network: &monero::Network,
    wallet_mutex: Arc<Mutex<monero_rpc::WalletClient>>,
    restore_height: Option<u64>,
    wallet_dir_path: Option<PathBuf>,
) -> Result<Vec<Vec<u8>>, Error> {
    let keypair = monero::KeyPair { view, spend };
    let password = s!(" ");
    let source_address = monero::Address::from_keypair(*network, &keypair);
    let wallet_filename = format!("sweep:{}", source_address);

    let wallet = wallet_mutex.lock().await;
    trace!("taking sweep wallet lock");

    while let Err(err) = wallet
        .open_wallet(wallet_filename.clone(), Some(password.clone()))
        .await
    {
        debug!(
            "error opening to be sweeped wallet: {:?}, falling back to generating a new wallet",
            err,
        );
        wallet
            .generate_from_keys(GenerateFromKeysArgs {
                restore_height,
                filename: wallet_filename.clone(),
                address: source_address,
                spendkey: Some(keypair.spend),
                viewkey: keypair.view,
                password: password.clone(),
                autosave_current: Some(true),
            })
            .await?;
    }

    // failsafe to check if the wallet really supports spending
    wallet.query_key(PrivateKeyType::Spend).await?;
    let (account, addrs) = (0, None);
    wallet.refresh(restore_height).await?;
    let balance = wallet.get_balance(account, addrs).await?;
    // only sweep once all the balance is unlocked
    if balance.unlocked_balance >= minimum_balance {
        info!(
            "Sweeping address {} with unlocked balance {} into {}",
            source_address.addr(),
            balance.unlocked_balance.bright_white_bold(),
            destination_address.addr(),
        );
        let sweep_args = monero_rpc::SweepAllArgs {
            address: destination_address,
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
                info!(
                    "Sweep transaction hash: {}",
                    hash_str.bright_yellow_italic()
                );
                hex::decode(hash_str).ok()
            })
            .collect();

        // close the wallet since we are done with it now
        wallet.close_wallet().await?;

        // delete all the associated wallet data if possible
        if let Some(path) = wallet_dir_path {
            if let Some(raw_path) = path.to_str() {
                let watch_wallet_filename = format!("/watch:{}", source_address);
                let sweep_wallet_filename = format!("/sweep:{}", source_address);
                if let Err(error) = fs::remove_file(raw_path.to_string() + &sweep_wallet_filename)
                    .and(fs::remove_file(
                        raw_path.to_string() + &sweep_wallet_filename + ".keys",
                    ))
                {
                    warn!("Failed to clean sweep wallet data after successful sweep. {}. The path used for the wallet directory is probably malformed", error);
                } else {
                    info!("Successfully removed sweep wallet data after completed sweep.");
                }

                if let Err(error) = fs::remove_file(raw_path.to_string() + &watch_wallet_filename)
                    .and(fs::remove_file(
                        raw_path.to_string() + &watch_wallet_filename + ".address.txt",
                    ))
                    .and(fs::remove_file(
                        raw_path.to_string() + &watch_wallet_filename + ".keys",
                    ))
                {
                    debug!("Failed to clean watch-only wallet data after sweep. {}. The path used for the wallet directory is probably malformed", error);
                } else {
                    debug!("Successfully removed watch-only wallet data after completed sweep");
                }
            } else {
                warn!("No associated wallet data cleaned up after sweep. The path used for the wallet directory is probably malformed");
            }
        } else {
            info!("{}", format!("Completed operations on Monero wallets with address {} . These wallets can now be safely deleted", source_address));
        }
        Ok(tx_ids)
    } else {
        debug!(
            "retrying sweep, balance not unlocked yet. Unlocked balance {:?}. Total balance {:?}. Expected balance {:?}.",
            balance.unlocked_balance, balance.balance, minimum_balance
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
    tokio::spawn(async move {
        loop {
            // this is a hack around the Receiver not being Sync
            let syncerd_task = receive_task_channel.try_recv();
            match syncerd_task {
                Ok(syncerd_task) => {
                    match syncerd_task.task {
                        Task::GetTx(_) => {
                            error!("get tx not implemented for monero syncer");
                        }
                        Task::WatchEstimateFee(_) => {
                            error!("estimate fee not implemented for monero syncer");
                        }
                        Task::SweepAddress(task) => match task.addendum.clone() {
                            SweepAddressAddendum::Monero(sweep) => {
                                let addr = sweep.destination_address;
                                debug!("Sweeping address: {}", addr.addr());
                                let mut state_guard = state.lock().await;
                                state_guard.sweep_address(task, syncerd_task.source);
                            }
                            _ => {
                                error!("Aborting sweep address task - unable to decode sweep address addendum");
                                let mut state_guard = state.lock().await;
                                state_guard
                                    .abort(TaskTarget::TaskId(task.id), syncerd_task.source, true)
                                    .await;
                            }
                        },
                        Task::Abort(task) => {
                            let mut state_guard = state.lock().await;
                            let respond = match task.respond {
                                Boolean::True => true,
                                Boolean::False => false,
                            };
                            state_guard
                                .abort(task.task_target, syncerd_task.source, respond)
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
                                state_guard.watch_address(task, syncerd_task.source);
                            }
                            _ => {
                                error!("Aborting watch address task - unable to decode address addendum");
                                let mut state_guard = state.lock().await;
                                state_guard
                                    .abort(TaskTarget::TaskId(task.id), syncerd_task.source, true)
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
                        Task::Terminate => {
                            debug!("unimplemented");
                        }
                    }
                    continue;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    panic!("Task receiver is disconnected, will exit synclet runtime")
                }
                Err(TryRecvError::Empty) => {
                    // do nothing
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
}

async fn subscribe_address_lws(
    address_addendum: XmrAddressAddendum,
    network: monero::Network,
    monero_lws_url: String,
) -> Result<(), Error> {
    let keypair = monero::ViewPair {
        spend: address_addendum.spend_key,
        view: address_addendum.view_key,
    };
    let address = monero::Address::from_viewpair(network, &keypair);
    let daemon_client = monero_lws::LwsRpcClient::new(monero_lws_url);
    debug!("subscribing monero address: {}, {:?}", address, address);
    let res = daemon_client
        .login(address, keypair.view, true, true)
        .await?;
    debug!("account created: {:?}", res);
    let res = daemon_client
        .login(address, keypair.view, false, false)
        .await?;
    debug!("logged in to lws: {:?}", res);
    let res = daemon_client
        .import_request(address, keypair.view, Some(address_addendum.from_height))
        .await?;
    debug!("import request to lws: {:?}", res);
    Ok(())
}

fn address_polling(
    state: Arc<Mutex<SyncerState>>,
    syncer_servers: MoneroSyncerServers,
    network: monero::Network,
    wallet_mutex: Arc<Mutex<monero_rpc::WalletClient>>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        let mut rpc = MoneroRpc::new(syncer_servers.monero_daemon);
        loop {
            let state_guard = state.lock().await;
            let mut addresses = state_guard.addresses.clone();
            let subscribed_addresses = state_guard.subscribed_addresses.clone();
            drop(state_guard);
            let mut needs_resubscribe = false;
            for (id, watched_address) in addresses.drain() {
                let address_addendum = match watched_address.task.addendum.clone() {
                    AddressAddendum::Monero(address) => address,
                    _ => panic!("should never get an invalid address"),
                };
                let address_transactions = if let Some(monero_lws) =
                    syncer_servers.monero_lws.clone()
                {
                    if needs_resubscribe {
                        needs_resubscribe = false;
                        let mut state_guard = state.lock().await;
                        state_guard.subscribed_addresses = none!();
                        drop(state_guard);
                    }
                    if !subscribed_addresses.contains(&watched_address.task.addendum) {
                        let success = match subscribe_address_lws(
                            address_addendum.clone(),
                            network,
                            monero_lws.clone(),
                        )
                        .await
                        {
                            Ok(()) => {
                                debug!("success subscribing address to monero lws.");
                                true
                            }
                            Err(err) => {
                                warn!("error subscribing address to monero lws: {:?}", err);
                                false
                            }
                        };
                        if success {
                            let mut state_guard = state.lock().await;
                            state_guard.address_subscribed(id);
                            drop(state_guard);
                        } else {
                            // an error might indicate that the remote server shutdown, so we should re-subscribe everything on re-connect
                            needs_resubscribe = true;
                            continue;
                        }
                    }
                    match rpc
                        .check_address_lws(address_addendum.clone(), network, monero_lws)
                        .await
                    {
                        Ok(address_transactions) => Some(address_transactions),
                        Err(err) => {
                            // an error might indicate that the remote server shutdown, so we should re-subscribe everything on re-connect
                            needs_resubscribe = true;
                            error!("error polling addresses: {:?}", err);
                            // the remote server may have disconnected, set the subscribed addresses to none
                            None
                        }
                    }
                } else {
                    // we cannot parallelize polling here, since we have to open and close the
                    // wallet
                    match rpc
                        .check_address(address_addendum.clone(), network, Arc::clone(&wallet_mutex))
                        .await
                    {
                        Ok(address_transactions) => Some(address_transactions),
                        Err(err) => {
                            error!("error polling addresses: {:?}", err);
                            None
                        }
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
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        }
    })
}

fn height_polling(
    state: Arc<Mutex<SyncerState>>,
    syncer_servers: MoneroSyncerServers,
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
                            .change_transaction(tx.tx_id, tx.block_hash, tx.confirmations, vec![])
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
    wallet_dir_path: Option<PathBuf>,
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
                        addendum.destination_address,
                        addendum.source_view_key,
                        addendum.source_spend_key,
                        addendum.minimum_balance,
                        &network,
                        Arc::clone(&wallet),
                        sweep_address_task.from_height,
                        wallet_dir_path.clone(),
                    )
                    .await
                    .unwrap_or_else(|err| {
                        warn!("error polling sweep address {:?}, retrying", err);
                        vec![]
                    });
                    let mut state_guard = state.lock().await;
                    if !sweep_address_txs.is_empty() {
                        state_guard.success_sweep(id, sweep_address_txs).await;
                    } else if !sweep_address_task.retry {
                        state_guard.fail_sweep(id).await;
                    }
                    drop(state_guard);
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    })
}

fn unseen_transaction_polling(
    state: Arc<Mutex<SyncerState>>,
    syncer_servers: MoneroSyncerServers,
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
                        .change_transaction(tx.tx_id, tx.block_hash, tx.confirmations, vec![])
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
        let mut connection = Connection::with_socket(ZmqSocketType::Push, tx);
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
                .expect("failed to send over zmq socket to the bridge");
        }
    });
}

/// Specific Monero configuration
#[derive(Default, Debug, Clone, Eq, PartialEq, Hash)]
pub struct MoneroSyncerServers {
    /// Monero daemon to use
    pub monero_daemon: String,

    /// Monero rpc wallet to use
    pub monero_rpc_wallet: String,

    /// Monero lws to use
    pub monero_lws: Option<String>,
}

impl Synclet for MoneroSyncer {
    fn run(
        &mut self,
        receive_task_channel: Receiver<SyncerdTask>,
        tx: zmq::Socket,
        syncer_address: Vec<u8>,
        opts: &Opts,
        network: Network,
        polling: bool,
    ) -> Result<(), Error> {
        if !polling {
            warn!("monero syncer only supports polling for now - switching to polling=true");
        }
        let network = network.into();
        if let Some(daemon) = &opts.monero_daemon {
            if let Some(rpc_wallet) = &opts.monero_rpc_wallet {
                let syncer_servers = MoneroSyncerServers {
                    monero_daemon: daemon.clone(),
                    monero_rpc_wallet: rpc_wallet.clone(),
                    monero_lws: opts.monero_lws.clone(),
                };
                info!("monero syncer servers: {:?}", syncer_servers);
                let wallet_dir = opts.monero_wallet_dir_path.clone().map(PathBuf::from);

                let _handle = std::thread::spawn(move || {
                    use tokio::runtime::Builder;
                    let rt = Builder::new_multi_thread()
                        .worker_threads(2)
                        .enable_all()
                        .build()
                        .unwrap();
                    rt.block_on(async {
                        let wallet_mutex = Arc::new(Mutex::new(
                            monero_rpc::RpcClient::new(syncer_servers.monero_rpc_wallet.clone())
                                .wallet(),
                        ));
                        let (event_tx, event_rx): (
                            TokioSender<SyncerdBridgeEvent>,
                            TokioReceiver<SyncerdBridgeEvent>,
                        ) = tokio::sync::mpsc::channel(120);
                        let state = Arc::new(Mutex::new(SyncerState::new(
                            event_tx.clone(),
                            Blockchain::Monero,
                        )));

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
                        let height_handle =
                            height_polling(Arc::clone(&state), syncer_servers.clone());

                        let unseen_transaction_handle =
                            unseen_transaction_polling(Arc::clone(&state), syncer_servers.clone());

                        let sweep_handle = sweep_polling(
                            Arc::clone(&state),
                            Arc::clone(&wallet_mutex),
                            network,
                            wallet_dir,
                        );

                        let res = tokio::try_join!(
                            address_handle,
                            height_handle,
                            unseen_transaction_handle,
                            sweep_handle
                        );
                        debug!("exiting monero synclet run routine with: {:?}", res);
                    });
                });
                Ok(())
            } else {
                error!("Missing --monero-rpc-wallet argument");
                Err(SyncerError::InvalidConfig.into())
            }
        } else {
            error!("Missing --monero-daemon argument");
            Err(SyncerError::InvalidConfig.into())
        }
    }
}
