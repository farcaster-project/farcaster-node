// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use crate::bus::info::Address;
use crate::bus::sync::{BridgeEvent, SyncMsg};
use crate::bus::{AddressSecretKey, BusMsg};
use crate::error::SyncerError;
use crate::syncerd::opts::Opts;
use crate::syncerd::runtime::SyncerdTask;
use crate::syncerd::runtime::Synclet;
use crate::syncerd::syncer_state::{AddressTx, BalanceServiceIdPair, TransactionServiceIdPair};
use crate::syncerd::syncer_state::{GetTxServiceIdPair, SyncerState};
use crate::syncerd::types::{AddressAddendum, SweepAddressAddendum, Task};
use crate::syncerd::BtcAddressAddendum;
use crate::syncerd::Event;
use crate::syncerd::FeeEstimations;
use crate::syncerd::GetTx;
use crate::syncerd::Health;
use crate::syncerd::TaskTarget;
use crate::syncerd::TransactionBroadcasted;
use crate::syncerd::TransactionRetrieved;
use crate::syncerd::{AddressBalance, BroadcastTransaction};
use crate::{error::Error, syncerd::syncer_state::create_set};
use crate::{LogStyle, ServiceId};
use bitcoin::hashes::hex::ToHex;
use bitcoin::BlockHash;
use bitcoin::Script;
use electrum_client::{
    Client, ConfigBuilder, ElectrumApi, HeaderNotification, Hex32Bytes, Socks5Config,
};
use farcaster_core::bitcoin::segwitv0::signature_hash;
use farcaster_core::bitcoin::transaction::TxInRef;
use farcaster_core::blockchain::{Blockchain, Network};
use hex;
use internet2::session::LocalSession;
use internet2::zeromq::ZmqSocketType;
use internet2::SendRecvMessage;
use internet2::TypedEnum;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::Receiver as TokioReceiver;
use tokio::sync::mpsc::Sender as TokioSender;
use tokio::sync::Mutex;

use super::HealthCheck;
use super::{TxFilter, Txid};

const RETRY_TIMEOUT: u64 = 5;
const PING_WAIT: u8 = 2;

pub struct ElectrumRpc {
    client: Client,
    height: u64,
    block_hash: BlockHash,
    addresses: HashMap<BtcAddressAddendum, (Option<Hex32Bytes>, TxFilter)>,
    ping_count: u8,
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

fn create_electrum_client(
    electrum_server: &str,
    proxy_address: Option<String>,
) -> Result<Client, electrum_client::Error> {
    let config = ConfigBuilder::new().retry(0);

    if let Some(proxy_address) = proxy_address {
        let proxy = Socks5Config::new(proxy_address);
        Client::from_config(electrum_server, config.socks5(Some(proxy)).unwrap().build())
    } else {
        Client::from_config(electrum_server, config.build())
    }
}

impl ElectrumRpc {
    fn new(
        electrum_server: &str,
        proxy_address: Option<String>,
    ) -> Result<Self, electrum_client::Error> {
        debug!("creating ElectrumRpc client");
        let client = create_electrum_client(electrum_server, proxy_address)?;
        let header = client.block_headers_subscribe()?;
        debug!("New ElectrumRpc at height {}", header.height);

        Ok(Self {
            client,
            addresses: none!(),
            height: header.height as u64,
            block_hash: header.header.block_hash(),
            ping_count: 0,
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
        filter: TxFilter,
    ) -> Result<AddressNotif, Error> {
        debug!("attempting subscribing to: {}", address_addendum.address);

        if !self.addresses.contains_key(&address_addendum) {
            debug!("subscribing to: {}", address_addendum.address);
            let script_status = self
                .client
                .script_subscribe(&address_addendum.address.script_pubkey())?;
            self.addresses
                .insert(address_addendum.clone(), (script_status, filter.clone()));
            debug!(
                "registering address {} with script_status {:?}",
                &address_addendum.address,
                &script_status.map(|h| hex::encode(h.to_vec()))
            );
        }
        let txs = query_addr_history(&mut self.client, &address_addendum, &filter)?;
        logging(&txs, &address_addendum);
        let notif = AddressNotif {
            address: address_addendum,
            txs,
        };
        Ok(notif)
    }

    pub fn new_block_check(&mut self) -> Result<Vec<Block>, Error> {
        let mut blocks = vec![];
        while let Ok(Some(HeaderNotification { height, header })) = self.client.block_headers_pop()
        {
            self.height = height as u64;
            self.block_hash = header.block_hash();
            trace!("new height received: {}", self.height);
            blocks.push(Block {
                height: self.height,
                block_hash: self.block_hash,
            });
        }
        Ok(blocks)
    }

    /// check if a subscribed address received a new transaction
    pub fn address_change_check(&mut self) -> Vec<AddressNotif> {
        let mut notifs: Vec<AddressNotif> = vec![];
        for (address, (previous_status, filter)) in self.addresses.clone().into_iter() {
            // get pending notifications for this address/script_pubkey
            let script_pubkey = &address.address.script_pubkey();
            while let Ok(Some(script_status)) = self.client.script_pop(script_pubkey) {
                if Some(script_status) != previous_status {
                    if self
                        .addresses
                        .insert(address.clone(), (Some(script_status), filter.clone()))
                        .is_some()
                    {
                        debug!(
                            "updated address {} with script_status {}",
                            address.address,
                            hex::encode(script_status.to_vec())
                        );
                    } else {
                        debug!(
                            "registering address {} with script_status {}",
                            address.address,
                            hex::encode(script_status.to_vec())
                        );
                    }
                    match query_addr_history(&mut self.client, &address, &filter) {
                        Ok(txs) => {
                            debug!("creating AddressNotif");
                            logging(&txs, &address);
                            let new_notif = AddressNotif {
                                address: address.clone(),
                                txs,
                            };
                            debug!("creating address notifications");
                            notifs.push(new_notif);
                        }
                        Err(err) => {
                            debug!("Error querying address history: {}", err);
                        }
                    }
                } else {
                    debug!("state did not change for given address");
                }
            }
        }
        notifs
    }

    async fn query_transactions(&self, state: Arc<Mutex<SyncerState>>, unseen: bool) {
        let state_guard = state.lock().await;
        let txids: Vec<Txid> = if unseen {
            state_guard
                .unseen_transactions
                .iter()
                .map(|task_id| state_guard.transactions[task_id].task.hash)
                .collect()
        } else {
            state_guard
                .transactions
                .values()
                .map(|watched_tx| watched_tx.task.hash)
                .collect()
        };
        drop(state_guard);
        for tx_id in txids.iter() {
            let tx_id = match tx_id {
                Txid::Bitcoin(tx_id) => tx_id,
                Txid::Monero(tx_id) => {
                    error!(
                        "This is Monero txid, but expected a Bitcoin txid: {}",
                        tx_id
                    );
                    continue;
                }
            };
            // Get the full transaction
            match self.client.transaction_get(tx_id) {
                Ok(tx) => {
                    debug!("Updated tx: {}", tx_id);
                    // Look for history of the first output (maybe last is generally less likely
                    // to be used multiple times, so more efficient?!). If the history call
                    // fails or the transaction is not found in the history it is treated as unconfirmed.
                    let height = match self
                        .client
                        .script_get_history(&tx.output[0].script_pubkey)
                        .map_err(SyncerError::Electrum)
                        .and_then(|mut history| {
                            history
                                .iter()
                                .position(|history_entry| history_entry.tx_hash == *tx_id)
                                .map(|pos| history.remove(pos))
                                .ok_or(SyncerError::TxNotInHistory)
                        }) {
                        Ok(entry) => entry.height,
                        Err(err) => {
                            debug!(
                                "error getting script history for {}, treating as unconfirmed: {}",
                                &tx_id, err
                            );
                            let mut state_guard = state.lock().await;
                            state_guard
                                .change_transaction(
                                    (*tx_id).into(),
                                    None,
                                    Some(0),
                                    bitcoin::consensus::serialize(&tx),
                                )
                                .await;
                            drop(state_guard);
                            continue;
                        }
                    };

                    let (conf_in_block, blockhash) = match height {
                        // Transaction unconfirmed (0 or -1)
                        i32::MIN..=0 => (None, None),
                        // Transaction confirmed at this height
                        1.. => {
                            // SAFETY: safe cast as it strictly greater than 0
                            let confirm_height = height as usize;
                            let block = match self.client.block_header(confirm_height) {
                                Ok(block) => block,
                                Err(err) => {
                                    debug!(
                                        "error getting block header, treating as unconfirmed: {}",
                                        err
                                    );
                                    let mut state_guard = state.lock().await;
                                    state_guard
                                        .change_transaction(
                                            (*tx_id).into(),
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
                                "error getting top block header, treating as unconfirmed: {}",
                                err
                            );
                            let mut state_guard = state.lock().await;
                            state_guard
                                .change_transaction(
                                    (*tx_id).into(),
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
                            tx.txid().into(),
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
                        .change_transaction((*tx_id).into(), None, None, vec![])
                        .await;
                    drop(state_guard);
                }
            }
        }
    }
}

fn query_addr_history(
    client: &mut Client,
    address: &BtcAddressAddendum,
    filter: &TxFilter,
) -> Result<Vec<AddressTx>, Error> {
    // now that we have established _something_ has changed get the full transaction
    // history of the address
    let script_pubkey = address.address.script_pubkey();
    let tx_hist = client.script_get_history(&script_pubkey)?;
    trace!("history: {:?}", tx_hist);

    let mut addr_txs = vec![];
    for hist in tx_hist {
        let txid = hist.tx_hash;
        let tx = client.transaction_get(&txid)?;
        let mut output_found = false;
        let mut input_found = false;
        let mut in_amount: u64 = 0;
        let mut out_amount: u64 = 0;
        for output in tx.output.iter() {
            if output.script_pubkey == script_pubkey {
                output_found = true;
                in_amount += output.value;
            } else {
                // since we're filtering for a pubkey's history, if the tx's
                // output's pubkey is _not_ the pubkey we're filtering for, we can infer
                // from the fact that the tx is related to the pubkey that the tx
                // must be spending _from_ the pubkey
                out_amount += output.value;
            }
        }
        for input in tx.input.iter() {
            let prev_tx = match client.transaction_get(&input.previous_output.txid) {
                Ok(tx) => tx,
                Err(_) => {
                    trace!(
                        "Input transaction not found, this is probably a coinbase tx, skipping."
                    );
                    break;
                }
            };
            for output in prev_tx.output.iter() {
                if output.script_pubkey == script_pubkey {
                    input_found = true;
                }
            }
        }

        let amount = match filter {
            TxFilter::Incoming => {
                if output_found {
                    in_amount
                } else {
                    debug!(
                        "Ignoring outgoing transaction {} in handle address notification, continuing", txid
                    );
                    continue;
                }
            }
            TxFilter::Outgoing => {
                if input_found {
                    out_amount
                } else {
                    debug!(
                        "Ignoring incoming transaction {} in handle address notification, continuing", txid
                    );
                    continue;
                }
            }
            TxFilter::All => {
                if output_found {
                    in_amount
                } else if input_found {
                    out_amount
                } else {
                    debug!(
                        "Ignoring transaction {} in handle address notifcation, continuing",
                        txid
                    );
                    continue;
                }
            }
        };
        addr_txs.push(AddressTx {
            amount,
            tx_id: txid.into(),
            tx: bitcoin::consensus::serialize(&tx),
            incoming: output_found && !input_found,
        })
    }
    Ok(addr_txs)
}

/// Returns the script code used for spending a P2WPKH output if this script is a script pubkey
/// for a P2WPKH output. The `scriptCode` is described in [BIP143].
///
/// [BIP143]: <https://github.com/bitcoin/bips/blob/99701f68a88ce33b2d0838eb84e115cef505b4c2/bip-0143.mediawiki>
/// FIXME: switch to impl in https://github.com/rust-bitcoin/rust-bitcoin/commit/d882b68a2cbba8e643695552f38e4b36558f9617 once released
fn p2wpkh_script_code(script: &bitcoin::Script) -> Script {
    bitcoin::blockdata::script::Builder::new()
        .push_opcode(bitcoin::blockdata::opcodes::all::OP_DUP)
        .push_opcode(bitcoin::blockdata::opcodes::all::OP_HASH160)
        .push_slice(&script[2..])
        .push_opcode(bitcoin::blockdata::opcodes::all::OP_EQUALVERIFY)
        .push_opcode(bitcoin::blockdata::opcodes::all::OP_CHECKSIG)
        .into_script()
}

fn sweep_address(
    source_secret_key: bitcoin::secp256k1::SecretKey,
    source_address: bitcoin::Address,
    dest_address: bitcoin::Address,
    client: &Client,
    network: bitcoin::Network,
) -> Result<Vec<Txid>, Error> {
    match source_address.address_type() {
        Some(bitcoin::AddressType::P2wpkh) => {}
        Some(address_type) => {
            return Err(Error::Farcaster(format!(
                "Sweeping addresses only supports native segwit v0 addresses. Address has type: {}",
                address_type
            )));
        }
        None => return Err(Error::Farcaster("Invalid to be swept address".to_string())),
    }

    let sk = bitcoin::PrivateKey::new(source_secret_key, network);
    let pk = bitcoin::PublicKey::from_private_key(bitcoin::secp256k1::SECP256K1, &sk);

    let unspent_txs = client.script_list_unspent(&source_address.script_pubkey())?;

    if unspent_txs.is_empty() {
        debug!(
            "No sweepable outputs detected for address: {}",
            source_address
        );
        return Ok(vec![]);
    }

    let in_amount = unspent_txs
        .iter()
        .fold(0, |acc, unspent_tx| acc + unspent_tx.value);
    let inputs: Vec<bitcoin::TxIn> = unspent_txs
        .iter()
        .map(|unspent_output| bitcoin::TxIn {
            previous_output: bitcoin::OutPoint {
                txid: unspent_output.tx_hash,
                vout: unspent_output.tx_pos as u32,
            },
            script_sig: bitcoin::Script::default(),
            sequence: (1 << 31) as u32,
            witness: bitcoin::Witness::new(),
        })
        .collect();

    let mut unsigned_tx = bitcoin::Transaction {
        version: 2,
        lock_time: 0,
        input: inputs,
        output: vec![bitcoin::TxOut {
            value: in_amount,
            script_pubkey: dest_address.script_pubkey(),
        }],
    };

    // TODO (maybe): make blocks_until_confirmation or fee_btc_per_kvb configurable by user (see FeeStrategy)
    let blocks_until_confirmation = 2;
    let fee_sat_per_kvb = (client
        // because near == far (target) low and high fee are equal
        .estimate_priority_fee(blocks_until_confirmation, blocks_until_confirmation)?
        .high_fee
        * 1.0e8)
        .ceil() as u64;
    let fee = p2wpkh_signed_tx_fee(fee_sat_per_kvb, unsigned_tx.vsize(), unspent_txs.len());

    // 546 is the dust limit for a p2pkh output. This covers both cases for when
    // a users provides a p2wpkh or p2pkh address
    if in_amount.saturating_sub(fee) <= 546 {
        warn!(
            "Amount is too close to being dust for address: {}, with total in amount {} and total fee {} ({} satoshi/kvb)",
            source_address, in_amount, fee, fee_sat_per_kvb,
        );
        return Ok(vec![]);
    }
    unsigned_tx.output[0].value = in_amount - fee;
    let mut psbt = bitcoin::util::psbt::PartiallySignedTransaction::from_unsigned_tx(unsigned_tx)
        .map_err(|_| Error::Syncer(SyncerError::InvalidPsbt))?;
    psbt.outputs[0].witness_script = Some(dest_address.script_pubkey());

    // sign the inputs and collect the witness data
    for (index, input) in psbt.inputs.iter_mut().enumerate() {
        input.witness_utxo = Some(bitcoin::TxOut {
            value: unspent_txs[index].value,
            script_pubkey: source_address.script_pubkey(),
        });
        let script = p2wpkh_script_code(&source_address.script_pubkey());
        input.witness_script = Some(script.clone());
        let txin = TxInRef::new(&psbt.unsigned_tx, index);
        let sig_hash = signature_hash(
            txin,
            &script,
            unspent_txs[index].value,
            bitcoin::EcdsaSighashType::All,
        );
        let message = bitcoin::secp256k1::Message::from_slice(&sig_hash)?;
        let signature = bitcoin::secp256k1::SECP256K1.sign_ecdsa(&message, &sk.inner);
        let sig_all = bitcoin::util::ecdsa::EcdsaSig::sighash_all(signature);
        input.partial_sigs.insert(pk, sig_all);
        input.final_script_witness = Some(bitcoin::Witness::from_vec(vec![
            sig_all.to_vec(),
            pk.to_bytes(),
        ]));
    }
    let finalized_signed_tx = psbt.extract_tx();
    let tx_hash =
        client.transaction_broadcast_raw(&bitcoin::consensus::serialize(&finalized_signed_tx))?;

    Ok(vec![tx_hash.into()])
}

async fn run_syncerd_bridge_event_sender(
    tx: zmq::Socket,
    mut event_rx: TokioReceiver<BridgeEvent>,
    syncer_address: Vec<u8>,
) {
    tokio::spawn(async move {
        let mut session = LocalSession::with_zmq_socket(ZmqSocketType::Push, tx);
        while let Some(event) = event_rx.recv().await {
            let request = BusMsg::Sync(SyncMsg::BridgeEvent(event));
            trace!("sending request over syncerd bridge: {}", request);
            session
                .send_routed_message(
                    &syncer_address,
                    &syncer_address,
                    &syncer_address,
                    &request.serialize(),
                )
                .expect("failed to send from bitcoin syncer to syncerd bridge");
        }
    });
}

async fn run_syncerd_task_receiver(
    electrum_server: String,
    proxy_address: Option<String>,
    receive_task_channel: Receiver<SyncerdTask>,
    state: Arc<Mutex<SyncerState>>,
    transaction_broadcast_tx: TokioSender<(BroadcastTransaction, ServiceId)>,
    transaction_get_tx: TokioSender<(GetTx, ServiceId)>,
    balance_get_tx: TokioSender<BalanceServiceIdPair>,
    terminate_tx: TokioSender<()>,
) {
    tokio::spawn(async move {
        loop {
            // this is a hack around the Receiver not being Sync
            let syncerd_task = receive_task_channel.try_recv();
            match syncerd_task {
                Ok(syncerd_task) => {
                    match syncerd_task.task {
                        Task::GetTx(task) => {
                            transaction_get_tx
                                .send((task, syncerd_task.source))
                                .await
                                .expect("failed on transaction_get sender");
                        }
                        Task::GetAddressBalance(task) => {
                            balance_get_tx
                                .send((task, syncerd_task.source))
                                .await
                                .expect("failed on balance_get sender");
                        }
                        Task::WatchEstimateFee(task) => {
                            let mut state_guard = state.lock().await;
                            state_guard.estimate_fee(task, syncerd_task.source).await;
                            drop(state_guard);
                        }
                        Task::SweepAddress(task) => match task.addendum.clone() {
                            SweepAddressAddendum::Bitcoin(sweep) => {
                                let addr = sweep.source_address;
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
                            state_guard
                                .abort(task.task_target, syncerd_task.source, task.respond)
                                .await;
                            drop(state_guard);
                        }
                        Task::BroadcastTransaction(task) => {
                            debug!("trying to broadcast tx: {}", task.tx.to_hex());
                            if let Some(height) = task.broadcast_after_height {
                                let mut state_guard = state.lock().await;
                                // If we already surpassed the height, immediately broadcast it. Otherwise queue the broadcast
                                if height <= state_guard.block_height() {
                                    drop(state_guard);
                                    transaction_broadcast_tx
                                        .send((task, syncerd_task.source))
                                        .await
                                        .expect("failed on transaction_broadcast_tx sender");
                                } else {
                                    state_guard
                                        .pending_broadcasts
                                        .insert((task, syncerd_task.source));
                                    drop(state_guard);
                                }
                            } else {
                                transaction_broadcast_tx
                                    .send((task, syncerd_task.source))
                                    .await
                                    .expect("failed on transaction_broadcast_tx sender");
                            }
                        }
                        Task::WatchAddress(task) => match task.addendum.clone() {
                            AddressAddendum::Bitcoin(_) => {
                                let mut state_guard = state.lock().await;
                                state_guard.watch_address(task.clone(), syncerd_task.source);
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
                            debug!("received new watch tx task for txid: {}", task.hash);
                            let mut state_guard = state.lock().await;
                            state_guard.watch_transaction(task, syncerd_task.source);
                            drop(state_guard);
                        }
                        Task::Terminate => {
                            debug!("terminating async syncer runtime");
                            terminate_tx
                                .send(())
                                .await
                                .expect("terminating, don't care if we panic");
                        }
                        Task::HealthCheck(HealthCheck { id }) => {
                            debug!("performing health check");
                            let health =
                                match ElectrumRpc::new(&electrum_server, proxy_address.clone())
                                    .and_then(|client| {
                                        client.client.ping()?;
                                        Ok(())
                                    }) {
                                    Err(err) => Health::FaultyElectrum(err.to_string()),
                                    Ok(_) => Health::Healthy,
                                };
                            let mut state_guard = state.lock().await;
                            state_guard
                                .health_result(id, health, syncerd_task.source)
                                .await;
                            drop(state_guard);
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
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    });
}

fn address_polling(
    state: Arc<Mutex<SyncerState>>,
    electrum_server: String,
    proxy_address: Option<String>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        loop {
            let mut rpc = match ElectrumRpc::new(&electrum_server, proxy_address.clone()) {
                Ok(client) => client,
                Err(err) => {
                    error!(
                        "failed to spawn electrum rpc client {} in address polling: {}",
                        &electrum_server, err
                    );
                    // wait a bit before retrying the connection
                    tokio::time::sleep(std::time::Duration::from_secs(RETRY_TIMEOUT)).await;
                    continue;
                }
            };

            loop {
                if let Err(err) = rpc.ping() {
                    error!("error ping electrum client in address polling: {}", err);
                    // break this loop and retry, since the electrum rpc client is probably
                    // broken
                    break;
                }
                let state_guard = state.lock().await;
                let addresses = state_guard.addresses.clone();
                drop(state_guard);
                for (id, address) in addresses.clone() {
                    if let AddressAddendum::Bitcoin(address_addendum) = address.task.addendum {
                        if !address.subscribed {
                            match rpc
                                .script_subscribe(address_addendum.clone(), address.task.filter)
                            {
                                Ok(notif) => {
                                    logging(&notif.txs, &address_addendum);
                                    let tx_set = create_set(notif.txs);
                                    let mut state_guard = state.lock().await;
                                    if let Some(address) = state_guard.addresses.get_mut(&id) {
                                        address.subscribed = true;
                                    }
                                    state_guard
                                        .change_address(
                                            AddressAddendum::Bitcoin(address_addendum.clone()),
                                            tx_set,
                                        )
                                        .await;
                                    drop(state_guard);
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
                        }
                    }
                }
                let mut addrs_notifs = rpc.address_change_check();
                if !addrs_notifs.is_empty() {
                    let mut state_guard = state.lock().await;
                    while let Some(AddressNotif { address, txs }) = addrs_notifs.pop() {
                        logging(&txs, &address);
                        state_guard
                            .change_address(AddressAddendum::Bitcoin(address), create_set(txs))
                            .await;
                    }
                    drop(state_guard);
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }

            // we need to un-subscribe all addresses first if we are creating a new client
            let mut state_guard = state.lock().await;
            state_guard.unsubscribe_addresses();
            drop(state_guard);
        }
    })
}

fn height_polling(
    state: Arc<Mutex<SyncerState>>,
    electrum_server: String,
    proxy_address: Option<String>,
    transaction_broadcast_tx: TokioSender<(BroadcastTransaction, ServiceId)>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        // outer loop ensures the polling restarts if there is an error
        loop {
            let mut rpc = match ElectrumRpc::new(&electrum_server, proxy_address.clone()) {
                Ok(client) => client,
                Err(err) => {
                    error!(
                        "failed to spawn electrum rpc client {} in height polling: {}",
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
                    error!("error ping electrum client in height polling: {}", err);
                    // break this loop and retry, since the electrum rpc client is probably
                    // broken
                    break;
                }
                let mut blocks = match rpc.new_block_check() {
                    Ok(blks) => blks,
                    Err(err) => {
                        error!("error polling bitcoin block height: {}", err);
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

                // if the blocks changed, check pending broadcasts and query transactions
                if block_change {
                    let state_guard = state.lock().await;
                    let height = state_guard.block_height();
                    let pending_broadcasts: HashSet<(BroadcastTransaction, ServiceId)> =
                        state_guard
                            .pending_broadcasts
                            .iter()
                            .filter(|(task, _)| {
                                if let Some(after_height) = task.broadcast_after_height {
                                    after_height < height
                                } else {
                                    false
                                }
                            })
                            .cloned()
                            .collect();
                    drop(state_guard);
                    for pending in pending_broadcasts {
                        // Do not re-try sending pending broadcasts
                        if let Err(err) = transaction_broadcast_tx.send(pending.clone()).await {
                            error!("error sending through transaction_broadcast_tx {}", err);
                        }
                        let mut state_guard = state.lock().await;
                        state_guard.pending_broadcasts.remove(&pending);
                        drop(state_guard);
                    }
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
    proxy_address: Option<String>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        // outer loop ensures the polling restarts if there is an error
        loop {
            let rpc = match ElectrumRpc::new(&electrum_server, proxy_address.clone()) {
                Ok(client) => client,
                Err(err) => {
                    error!(
                        "failed to spawn electrum rpc client ({}) in transaction polling: {}",
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
    proxy_address: Option<String>,
    mut transaction_broadcast_rx: TokioReceiver<(BroadcastTransaction, ServiceId)>,
    tx_event: TokioSender<BridgeEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        while let Some((broadcast_transaction, source)) = transaction_broadcast_rx.recv().await {
            debug!("creating transaction broadcast electrum client");
            match create_electrum_client(&electrum_server, proxy_address.clone()).and_then(
                |broadcast_client| {
                    broadcast_client.transaction_broadcast_raw(&broadcast_transaction.tx.clone())
                },
            ) {
                Ok(txid) => {
                    tx_event
                        .send(BridgeEvent {
                            event: Event::TransactionBroadcasted(TransactionBroadcasted {
                                id: broadcast_transaction.id,
                                tx: broadcast_transaction.tx,
                                error: None,
                            }),
                            source,
                        })
                        .await
                        .expect("error sending transaction broadcast event");
                    debug!("Successfully broadcasted: {}", txid.bright_yellow_italic());
                }
                Err(e) => {
                    tx_event
                        .send(BridgeEvent {
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

/// Result of querying electrum to get a low priority and high priority fee rate.
struct FeeByPriority {
    low_fee: f64,
    high_fee: f64,
}

/// Extend electrum client capabilities and query fee for low and high priority.
trait GenericEstimateFee {
    /// Query electrum for estimate fee for low and high priority or fall back on node's relay fee.
    fn estimate_priority_fee(
        &self,
        near_target: usize,
        far_target: usize,
    ) -> Result<FeeByPriority, electrum_client::Error>;
}

impl GenericEstimateFee for Client {
    fn estimate_priority_fee(
        &self,
        near_target: usize,
        far_target: usize,
    ) -> Result<FeeByPriority, electrum_client::Error> {
        let low_fee;
        let mut high_fee = self.estimate_fee(near_target)?;
        if high_fee == -1.0 {
            // None returned internally between node and electrum, fallback on relay_fee
            high_fee = self.relay_fee()?;
            low_fee = high_fee;
        } else {
            // Shortcut in case we want only 1 fee and near == far
            if far_target != near_target {
                low_fee = self.estimate_fee(far_target)?;
            } else {
                low_fee = high_fee
            }
        }
        Ok(FeeByPriority { low_fee, high_fee })
    }
}

fn estimate_fee_polling(
    electrum_server: String,
    proxy_address: Option<String>,
    state: Arc<Mutex<SyncerState>>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        let high_priority_target = 2;
        let low_priority_target = 6;
        loop {
            debug!("creating fee polling electrum client");
            if let Ok(client) = create_electrum_client(&electrum_server, proxy_address.clone()) {
                loop {
                    match client.estimate_priority_fee(high_priority_target, low_priority_target) {
                        Ok(FeeByPriority { low_fee, high_fee }) => {
                            let mut state_guard = state.lock().await;
                            state_guard
                                .fee_estimated(FeeEstimations::BitcoinFeeEstimation {
                                    high_priority_sats_per_kvbyte: (high_fee * 1.0e8).ceil() as u64,
                                    low_priority_sats_per_kvbyte: (low_fee * 1.0e8).ceil() as u64,
                                })
                                .await;
                            drop(state_guard);
                        }
                        Err(err) => {
                            error!("Failed to retrieve fee estimation: {}", err);
                            break;
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        }
    })
}

fn sweep_polling(
    state: Arc<Mutex<SyncerState>>,
    electrum_server: String,
    proxy_address: Option<String>,
    network: bitcoin::Network,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        loop {
            let state_guard = state.lock().await;
            let sweep_addresses = state_guard.sweep_addresses.clone();
            drop(state_guard);
            if !sweep_addresses.is_empty() {
                debug!("creating sweep polling electrum client");
                match create_electrum_client(&electrum_server, proxy_address.clone()) {
                    Err(err) => {
                        error!(
                            "Failed to create btc sweep electrum client: {}, retrying",
                            err
                        );
                    }
                    Ok(client) => {
                        for (id, sweep_address_task) in sweep_addresses.iter() {
                            if let SweepAddressAddendum::Bitcoin(addendum) =
                                sweep_address_task.addendum.clone()
                            {
                                let sweep_address_txids = sweep_address(
                                    addendum.source_secret_key,
                                    addendum.source_address,
                                    addendum.destination_address,
                                    &client,
                                    network,
                                )
                                .unwrap_or_else(|err| {
                                    warn!("error polling sweep address {}, retrying", err);
                                    vec![]
                                });
                                debug!(
                                    "sweep address transaction: {:?}",
                                    sweep_address_txids.iter().map(|txid| txid.to_string())
                                );
                                let mut state_guard = state.lock().await;
                                if !sweep_address_txids.is_empty() {
                                    state_guard.success_sweep(id, sweep_address_txids).await;
                                } else if !sweep_address_task.retry {
                                    state_guard.fail_sweep(id).await;
                                }
                                drop(state_guard);
                            } else {
                                error!("Not sweeping address - is not using a bitcoin sweep address addendum");
                            }
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    })
}

fn transaction_fetcher(
    electrum_server: String,
    proxy_address: Option<String>,
    mut transaction_get_rx: TokioReceiver<GetTxServiceIdPair>,
    tx_event: TokioSender<BridgeEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        while let Some((get_transaction, source)) = transaction_get_rx.recv().await {
            let tx_id = match get_transaction.hash {
                Txid::Bitcoin(tx_id) => tx_id,
                Txid::Monero(tx_id) => {
                    error!(
                        "This is a Monero txid, but expected a Bitcoin txid: {}",
                        tx_id
                    );
                    continue;
                }
            };
            debug!("creating transaction fetcher electrum client");
            match create_electrum_client(&electrum_server, proxy_address.clone())
                .and_then(|transaction_client| transaction_client.transaction_get(&tx_id))
            {
                Ok(tx) => {
                    tx_event
                        .send(BridgeEvent {
                            event: Event::TransactionRetrieved(TransactionRetrieved {
                                id: get_transaction.id,
                                tx: Some(tx),
                            }),
                            source,
                        })
                        .await
                        .expect("error sending transaction retrieved event");
                    debug!("successfully retrieved tx: {}", get_transaction.hash);
                }
                Err(e) => {
                    tx_event
                        .send(BridgeEvent {
                            event: Event::TransactionRetrieved(TransactionRetrieved {
                                id: get_transaction.id,
                                tx: None,
                            }),
                            source,
                        })
                        .await
                        .expect("error sending transaction retrieved event");
                    debug!("Error while retrieving tx {}: {}", get_transaction.hash, e);
                }
            }
        }
    })
}

fn balance_fetcher(
    electrum_server: String,
    proxy_address: Option<String>,
    mut balance_get_rx: TokioReceiver<BalanceServiceIdPair>,
    tx_event: TokioSender<BridgeEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn(async move {
        while let Some((get_balance, source)) = balance_get_rx.recv().await {
            let address = match get_balance.address_secret_key {
                AddressSecretKey::Monero { address, .. } => {
                    tx_event
                        .send(BridgeEvent {
                            event: Event::AddressBalance(AddressBalance {
                                address: Address::Monero(address),
                                id: get_balance.id,
                                balance: 0,
                                err: Some(
                                    "Sent monero address balance to bitcoin syncer".to_string(),
                                ),
                            }),
                            source,
                        })
                        .await
                        .expect("error sending address balance event");
                    warn!("Received monero address balance task in bitcoin syncer");
                    continue;
                }
                AddressSecretKey::Bitcoin { address, .. } => address,
            };

            debug!("creating balance fetcher electrum client");

            match create_electrum_client(&electrum_server, proxy_address.clone()).and_then(
                |transaction_client| {
                    transaction_client.script_get_balance(&address.script_pubkey())
                },
            ) {
                Ok(balance) => {
                    tx_event
                        .send(BridgeEvent {
                            event: Event::AddressBalance(AddressBalance {
                                id: get_balance.id,
                                address: Address::Bitcoin(address.clone()),
                                balance: balance.unconfirmed.unsigned_abs() + balance.confirmed,
                                err: None,
                            }),
                            source,
                        })
                        .await
                        .expect("error sending address balance event");
                    debug!(
                        "successfully retrieved balance: {} for address {}.",
                        balance.unconfirmed.unsigned_abs() + balance.confirmed,
                        address
                    );
                }
                Err(e) => {
                    tx_event
                        .send(BridgeEvent {
                            event: Event::AddressBalance(AddressBalance {
                                id: get_balance.id,
                                address: Address::Bitcoin(address.clone()),
                                balance: 0,
                                err: Some(e.to_string()),
                            }),
                            source,
                        })
                        .await
                        .expect("error sending address balance event");
                    warn!("failed to retrieve balance for address {}: {}", address, e);
                }
            }
        }
    })
}

fn terminate_polling(
    mut rx_terminate: TokioReceiver<()>,
) -> tokio::task::JoinHandle<Result<(), Error>> {
    tokio::task::spawn(async move {
        let _ = rx_terminate.recv().await;
        debug!("received terminate thread");
        panic!("terminate this thread");
    })
}

impl Synclet for BitcoinSyncer {
    fn run(
        &mut self,
        receive_task_channel: Receiver<SyncerdTask>,
        tx: zmq::Socket,
        syncer_address: Vec<u8>,
        opts: &Opts,
        network: Network,
    ) -> Result<(), Error> {
        let btc_network = network.into();
        let proxy_address = opts.shared.tor_proxy.map(|address| address.to_string());
        debug!("bitcoin synclet using proxy: {:?}", proxy_address);

        if let Some(electrum_server) = &opts.electrum_server {
            let electrum_server = electrum_server.clone();
            std::thread::spawn(move || {
                use tokio::runtime::Builder;
                trace!("building tokio syncer runtime");
                let rt = Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("failed to build tokio runtime");
                trace!("completed tokio syncer runtime");
                rt.block_on(async {
                    let (event_tx, event_rx): (
                        TokioSender<BridgeEvent>,
                        TokioReceiver<BridgeEvent>,
                    ) = tokio::sync::mpsc::channel(200);
                    let (transaction_broadcast_tx, transaction_broadcast_rx): (
                        TokioSender<TransactionServiceIdPair>,
                        TokioReceiver<TransactionServiceIdPair>,
                    ) = tokio::sync::mpsc::channel(200);
                    let (transaction_get_tx, transaction_get_rx): (
                        TokioSender<GetTxServiceIdPair>,
                        TokioReceiver<GetTxServiceIdPair>,
                    ) = tokio::sync::mpsc::channel(200);
                    let (balance_get_tx, balance_get_rx): (
                        TokioSender<BalanceServiceIdPair>,
                        TokioReceiver<BalanceServiceIdPair>,
                    ) = tokio::sync::mpsc::channel(200);
                    let (terminate_tx, terminate_rx): (TokioSender<()>, TokioReceiver<()>) =
                        tokio::sync::mpsc::channel(1);
                    let state = Arc::new(Mutex::new(SyncerState::new(
                        event_tx.clone(),
                        Blockchain::Bitcoin,
                    )));

                    run_syncerd_task_receiver(
                        electrum_server.clone(),
                        proxy_address.clone(),
                        receive_task_channel,
                        Arc::clone(&state),
                        transaction_broadcast_tx.clone(),
                        transaction_get_tx,
                        balance_get_tx,
                        terminate_tx,
                    )
                    .await;
                    run_syncerd_bridge_event_sender(tx, event_rx, syncer_address).await;

                    let address_handle = address_polling(
                        Arc::clone(&state),
                        electrum_server.clone(),
                        proxy_address.clone(),
                    );

                    let height_handle = height_polling(
                        Arc::clone(&state),
                        electrum_server.clone(),
                        proxy_address.clone(),
                        transaction_broadcast_tx,
                    );

                    let unseen_transaction_handle = unseen_transaction_polling(
                        Arc::clone(&state),
                        electrum_server.clone(),
                        proxy_address.clone(),
                    );

                    let transaction_broadcast_handle = transaction_broadcasting(
                        electrum_server.clone(),
                        proxy_address.clone(),
                        transaction_broadcast_rx,
                        event_tx.clone(),
                    );

                    let transaction_get_handle = transaction_fetcher(
                        electrum_server.clone(),
                        proxy_address.clone(),
                        transaction_get_rx,
                        event_tx.clone(),
                    );

                    let balance_get_handle = balance_fetcher(
                        electrum_server.clone(),
                        proxy_address.clone(),
                        balance_get_rx,
                        event_tx.clone(),
                    );

                    let estimate_fee_handle = estimate_fee_polling(
                        electrum_server.clone(),
                        proxy_address.clone(),
                        Arc::clone(&state),
                    );

                    let sweep_handle = sweep_polling(
                        Arc::clone(&state),
                        electrum_server.clone(),
                        proxy_address.clone(),
                        btc_network,
                    );

                    let terminate_handle = terminate_polling(terminate_rx);

                    let res = tokio::try_join!(
                        address_handle,
                        height_handle,
                        unseen_transaction_handle,
                        transaction_broadcast_handle,
                        transaction_get_handle,
                        balance_get_handle,
                        estimate_fee_handle,
                        sweep_handle,
                        terminate_handle,
                    );
                    debug!("exiting bitcoin synclet run routine with: {:?}", res);
                });
                debug!("shutting down runtime");
                rt.shutdown_timeout(Duration::from_millis(100));
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
            address.address,
            tx.tx_id.addr()
        );
    });
}

/// Input fee in sat_per_kvb, output fee in sat units
pub fn p2wpkh_signed_tx_fee(
    fee_sat_per_kvb: u64,
    unsigned_tx_vsize: usize,
    nr_inputs: usize,
) -> u64 {
    // Transaction size calculation: https://bitcoinops.org/en/tools/calc-size/
    // The items in the witness are discounted by a factor of 4 (witness discount)
    // The size used here is ceil(input p2wpkh witness)
    // Input Witness:= ceil(nr. of items field + (length field + signature + public key) / p2wpkh witness discount
    // Input Witness:= ceil(0.25               + (1         + 73       + 34) / 4))
    //              := ceil(27.25)
    let vsize_per_p2wpkh_input_witness = 28;
    let signed_tx_size = unsigned_tx_vsize + vsize_per_p2wpkh_input_witness * nr_inputs;
    let fee = fee_sat_per_kvb as f64 * signed_tx_size as f64 * 1e-3;
    // after multiplication we can safely convert
    fee.ceil() as u64
}
