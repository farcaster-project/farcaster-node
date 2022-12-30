// Copyright 2020-2022 Farcaster Devs & LNP/BP Standards Association
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use crate::{
    bus::ServiceBus,
    service::{Endpoints, LogStyle, SwapDetails, SwapLogging},
    syncerd::{
        Abort, AddressAddendum, BroadcastTransaction, BtcAddressAddendum, GetTx, SweepAddress,
        SweepAddressAddendum, SweepBitcoinAddress, SweepMoneroAddress, TaskTarget,
        TransactionBroadcasted, TxFilter, Txid, WatchAddress, WatchEstimateFee, WatchHeight,
        WatchTransaction, XmrAddressAddendum,
    },
    Error,
};
use bitcoin::consensus::Decodable;
use farcaster_core::{
    blockchain::Blockchain,
    role::{SwapRole, TradeRole},
    swap::SwapId,
    transaction::TxLabel,
};
use std::collections::HashMap;

use crate::{
    bus::sync::SyncMsg,
    bus::BusMsg,
    syncerd::{Task, TaskId},
    ServiceId,
};

pub struct SyncerTasks {
    pub counter: u32,
    pub watched_txs: HashMap<TaskId, TxLabel>,
    pub final_txs: HashMap<TxLabel, bool>,
    pub watched_addrs: HashMap<TaskId, TxLabel>,
    pub retrieving_txs: HashMap<TaskId, TxLabel>,
    pub broadcasting_txs: HashMap<TaskId, TxLabel>,
    pub sweeping_addr: Option<TaskId>,
    pub txids: HashMap<TxLabel, bitcoin::Txid>,
    pub tasks: HashMap<TaskId, Task>,
}

impl SyncerTasks {
    pub fn new_taskid(&mut self) -> TaskId {
        self.counter += 1;
        TaskId(self.counter)
    }
}

pub struct SyncerState {
    pub swap_id: SwapId,
    pub local_swap_role: SwapRole,
    pub local_trade_role: TradeRole,
    pub tasks: SyncerTasks,
    pub bitcoin_height: u64,
    pub monero_height: u64,
    pub confirmation_bound: u32,
    pub last_tx_event: HashMap<TxLabel, SyncMsg>,
    pub network: farcaster_core::blockchain::Network,
    pub bitcoin_syncer: ServiceId,
    pub monero_syncer: ServiceId,
    pub xmr_addr_addendum: Option<XmrAddressAddendum>,
    pub confirmations: HashMap<TxLabel, Option<u32>>,
    pub awaiting_funding: bool,
    pub broadcasted_txs: HashMap<TxLabel, bitcoin::Transaction>,
}

impl SwapLogging for SyncerState {
    fn swap_details(&self) -> SwapDetails {
        (
            Some(self.swap_id.0),
            Some(self.local_swap_role),
            Some(self.local_trade_role),
        )
    }
}

impl SyncerState {
    pub fn task_lifetime(&self, blockchain: Blockchain) -> u64 {
        let height = self.height(blockchain);
        if height > 0 {
            height + 500
        } else {
            u64::MAX
        }
    }
    pub fn bitcoin_syncer(&self) -> ServiceId {
        self.bitcoin_syncer.clone()
    }
    pub fn monero_syncer(&self) -> ServiceId {
        self.monero_syncer.clone()
    }
    pub fn height(&self, blockchain: Blockchain) -> u64 {
        match blockchain {
            Blockchain::Bitcoin => self.bitcoin_height,
            Blockchain::Monero => self.monero_height,
        }
    }
    pub fn handle_height_change(&mut self, new_height: u64, blockchain: Blockchain) {
        let height = match blockchain {
            Blockchain::Bitcoin => &mut self.bitcoin_height,
            Blockchain::Monero => &mut self.monero_height,
        };
        if &new_height > height {
            *height = new_height;
            self.log_debug(format!("{} new height {}", blockchain, &new_height));
        } else {
            self.log_warn("block height did not increment, maybe syncer sends multiple events");
        }
    }
    pub fn abort_task(&mut self, id: TaskId) -> Task {
        Task::Abort(Abort {
            task_target: TaskTarget::TaskId(id),
            respond: false,
        })
    }

    pub fn broadcasted_tx(&self, tx_label: &TxLabel) -> bool {
        self.broadcasted_txs.contains_key(tx_label)
            || self.tasks.broadcasting_txs.values().any(|l| l == tx_label)
    }

    pub fn estimate_fee_btc(&mut self) -> Task {
        let id = self.tasks.new_taskid();
        let task = Task::WatchEstimateFee(WatchEstimateFee {
            id,
            lifetime: self.task_lifetime(Blockchain::Bitcoin),
        });
        self.tasks.tasks.insert(id, task.clone());
        task
    }

    pub fn watch_tx_btc(&mut self, txid: bitcoin::Txid, tx_label: TxLabel) -> Task {
        if self.is_watched_tx(&tx_label) {
            self.log_warn(format!(
                "Already watching for tx with label {} - notifications will be repeated",
                tx_label.label()
            ));
        }
        let id = self.tasks.new_taskid();
        self.tasks.watched_txs.insert(id, tx_label);
        self.tasks.txids.insert(tx_label, txid);
        self.log_info(format!(
            "Watching {} transaction ({})",
            tx_label.label(),
            txid.tx_hash()
        ));
        let task = Task::WatchTransaction(WatchTransaction {
            id,
            lifetime: self.task_lifetime(Blockchain::Bitcoin),
            hash: txid.into(),
            confirmation_bound: self.confirmation_bound,
        });
        self.tasks.tasks.insert(id, task.clone());
        task
    }
    pub fn is_watched_tx(&self, tx_label: &TxLabel) -> bool {
        self.tasks.watched_txs.values().any(|tx| tx == tx_label)
    }
    pub fn watch_tx_xmr(&mut self, hash: Txid, tx_label: TxLabel) -> Task {
        if self.is_watched_tx(&tx_label) {
            self.log_warn(format!(
                "Already watching for tx with label {} - notifications will be repeated",
                tx_label.label()
            ));
        }
        let id = self.tasks.new_taskid();
        self.tasks.watched_txs.insert(id, tx_label);

        self.log_info(format!(
            "Watching {} transaction ({})",
            tx_label.label(),
            hash,
        ));
        self.log_debug(format!("Watching transaction {} with {}", hash, id));
        let task = Task::WatchTransaction(WatchTransaction {
            id,
            lifetime: self.task_lifetime(Blockchain::Monero),
            hash,
            confirmation_bound: self.confirmation_bound,
        });
        self.tasks.tasks.insert(id, task.clone());
        task
    }
    pub fn retrieve_tx_btc(&mut self, txid: Txid, tx_label: TxLabel) -> Task {
        let id = self.tasks.new_taskid();
        let task = Task::GetTx(GetTx { id, hash: txid });
        self.tasks.retrieving_txs.insert(id, tx_label);
        self.tasks.tasks.insert(id, task.clone());
        task
    }
    pub fn watch_addr_btc(&mut self, address: bitcoin::Address, tx_label: TxLabel) -> Task {
        if self.is_watched_addr(&tx_label) {
            self.log_warn(format!(
                "Address already watched for {} - notifications will be repeated",
                tx_label.label()
            ));
        }
        let id = self.tasks.new_taskid();
        self.tasks.watched_addrs.insert(id, tx_label);
        self.log_info(format!(
            "Watching {} on address {}",
            tx_label.label(),
            address.addr(),
        ));
        let addendum = BtcAddressAddendum { address };
        let filter = if TxLabel::Cancel == tx_label {
            // If this is the cancel transaction, only look for outgoing transactions
            TxFilter::Outgoing
        } else {
            TxFilter::Incoming
        };
        let task = Task::WatchAddress(WatchAddress {
            id,
            lifetime: self.task_lifetime(Blockchain::Bitcoin),
            addendum: AddressAddendum::Bitcoin(addendum),
            include_tx: true,
            filter,
        });
        self.tasks.tasks.insert(id, task.clone());
        task
    }

    pub fn is_watched_addr(&self, tx_label: &TxLabel) -> bool {
        self.tasks.watched_addrs.values().any(|tx| tx == tx_label)
    }

    /// Watches an xmr address from provided height.
    pub fn watch_addr_xmr(
        &mut self,
        address: monero::Address,
        view: monero::PrivateKey,
        tx_label: TxLabel,
        from_height: u64,
    ) -> Task {
        if self.is_watched_addr(&tx_label) {
            self.log_warn(format!(
                "Address {} already watched for {} - notifications will be repeated",
                address,
                tx_label.label()
            ));
        }
        self.log_debug(format!(
            "Address {} secret view key for {}: {}",
            address,
            tx_label.bright_white_bold(),
            view.bright_white_italic()
        ));
        let addendum = XmrAddressAddendum {
            address,
            view_key: view,
            from_height,
        };

        self.xmr_addr_addendum = Some(addendum.clone());

        let id = self.tasks.new_taskid();
        self.tasks.watched_addrs.insert(id, tx_label);

        self.log_info(format!(
            "Watching {} on address {} from height {} - current height {}",
            tx_label.label(),
            address.addr(),
            from_height,
            self.monero_height,
        ));

        let watch_addr = WatchAddress {
            id,
            lifetime: self.task_lifetime(Blockchain::Monero),
            addendum: AddressAddendum::Monero(addendum),
            include_tx: false,
            filter: TxFilter::Incoming,
        };
        let task = Task::WatchAddress(watch_addr);
        self.tasks.tasks.insert(id, task.clone());
        task
    }

    pub fn watch_height(
        &mut self,
        endpoints: &mut Endpoints,
        blockchain: Blockchain,
    ) -> Result<(), Error> {
        let swap_id = ServiceId::Swap(self.swap_id);
        let task_id = self.tasks.new_taskid();
        self.log_trace(format!("Watch height {}", blockchain));
        let task = Task::WatchHeight(WatchHeight {
            id: task_id,
            lifetime: self.task_lifetime(blockchain),
        });
        self.tasks.tasks.insert(task_id, task.clone());
        endpoints.send_to(
            ServiceBus::Sync,
            swap_id,
            match blockchain {
                Blockchain::Bitcoin => self.bitcoin_syncer(),
                Blockchain::Monero => self.monero_syncer(),
            },
            BusMsg::Sync(SyncMsg::Task(task)),
        )?;
        Ok(())
    }

    pub fn sweep_btc(&mut self, addendum: SweepBitcoinAddress, retry: bool) -> Task {
        let id = self.tasks.new_taskid();
        self.tasks.sweeping_addr = Some(id);
        let lifetime = self.task_lifetime(Blockchain::Bitcoin);
        let sweep_task = SweepAddress {
            id,
            lifetime,
            addendum: SweepAddressAddendum::Bitcoin(addendum),
            retry,
        };
        let task = Task::SweepAddress(sweep_task);
        self.tasks.tasks.insert(id, task.clone());
        task
    }

    pub fn sweep_xmr(&mut self, addendum: SweepMoneroAddress, retry: bool) -> Task {
        let id = self.tasks.new_taskid();
        self.tasks.sweeping_addr = Some(id);
        let lifetime = self.task_lifetime(Blockchain::Monero);
        let sweep_task = SweepAddress {
            id,
            lifetime,
            addendum: SweepAddressAddendum::Monero(addendum),
            retry,
        };
        let task = Task::SweepAddress(sweep_task);
        self.tasks.tasks.insert(id, task.clone());
        task
    }

    pub fn broadcast(&mut self, tx: bitcoin::Transaction, label: TxLabel) -> Task {
        let id = self.tasks.new_taskid();
        let task = Task::BroadcastTransaction(BroadcastTransaction {
            id,
            tx: bitcoin::consensus::serialize(&tx),
            broadcast_after_height: None,
        });
        self.tasks.tasks.insert(id, task.clone());
        self.tasks.broadcasting_txs.insert(id, label);
        task
    }
    pub fn transaction_broadcasted(&mut self, event: &TransactionBroadcasted) {
        if let Some(txlabel) = self.tasks.broadcasting_txs.remove(&event.id) {
            self.tasks.tasks.remove(&event.id);
            if let Some(ref err) = event.error {
                self.log_warn(format!(
                    "Error broadcasting {} transaction: {}",
                    txlabel, err
                ));
            } else {
                let tx = match bitcoin::Transaction::consensus_decode(std::io::Cursor::new(
                    event.tx.clone(),
                )) {
                    Ok(tx) => tx,
                    Err(_) => {
                        self.log_warn(format!(
                            "Error while consensus decoding broadcasted {} transaction",
                            txlabel
                        ));
                        return;
                    }
                };
                self.broadcasted_txs.insert(txlabel, tx);
            }
        }
    }
    pub fn pending_broadcast_txs(&self) -> Vec<(bitcoin::Transaction, TxLabel)> {
        self.tasks
            .broadcasting_txs
            .iter()
            .filter_map(|(id, label)| {
                if let Task::BroadcastTransaction(broadcast_tx) = self.tasks.tasks.get(id)? {
                    Some((
                        bitcoin::Transaction::consensus_decode(std::io::Cursor::new(
                            broadcast_tx.tx.clone(),
                        ))
                        .ok()?,
                        *label,
                    ))
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn acc_lock_watched(&self) -> bool {
        self.tasks
            .watched_addrs
            .values()
            .any(|&x| x == TxLabel::AccLock)
    }
    pub fn handle_tx_confs(
        &mut self,
        id: &TaskId,
        confirmations: &Option<u32>,
        swapid: SwapId,
        finality_thr: u32,
        endpoints: &mut Endpoints,
    ) {
        if let Some(txlabel) = self.tasks.watched_txs.get(id).cloned() {
            if !self.tasks.final_txs.contains_key(&txlabel)
                && confirmations.is_some()
                && confirmations.unwrap() >= finality_thr
            {
                self.log_info(format!(
                    "Tx {} {} with {} {}",
                    txlabel.label(),
                    "final".bright_green_bold(),
                    confirmations.unwrap().bright_green_bold(),
                    "confirmations".bright_green_bold()
                ));
                self.tasks.final_txs.insert(txlabel, true);
            } else if let Some(finality) = self.tasks.final_txs.get(&txlabel) {
                self.log_info(format!(
                    "Tx {} {}",
                    txlabel.label(),
                    if *finality {
                        "final".bright_green_bold()
                    } else {
                        "non-final".red_bold()
                    },
                ));
            } else {
                match confirmations {
                    Some(0) => {
                        self.log_info(format!(
                            "Tx {} on mempool but hasn't been mined",
                            txlabel.label()
                        ));
                    }
                    Some(confs) => {
                        self.log_info(format!(
                            "Tx {} mined with {} {}",
                            txlabel.label(),
                            confs.bright_green_bold(),
                            "confirmations".bright_green_bold(),
                        ));
                    }
                    None => {
                        if let Some(tx) = self.broadcasted_txs.get(&txlabel) {
                            let tx = tx.clone();
                            self.log_warn(format!("Tx {} was re-orged or dropped from the mempool. Re-broadcasting tx", txlabel.label()));
                            let task = self.broadcast(tx, txlabel);
                            if let Err(err) = endpoints.send_to(
                                ServiceBus::Sync,
                                ServiceId::Swap(swapid),
                                self.bitcoin_syncer(),
                                BusMsg::Sync(SyncMsg::Task(task)),
                            ) {
                                self.log_error(format!(
                                    "Failed to send task for re-broadcasting {} transaction: {}",
                                    txlabel, err
                                ));
                            }
                        }
                        self.log_info(format!("Tx {} not on the mempool", txlabel.label()));
                    }
                }
            }
            self.confirmations.insert(txlabel, *confirmations);
        } else {
            self.log_error(format!(
                "Received event with unknown transaction and task id {}",
                &id
            ));
        }
    }
    pub fn watch_bitcoin_fee(&mut self, endpoints: &mut Endpoints) -> Result<(), Error> {
        let identity = ServiceId::Swap(self.swap_id);
        let task = self.estimate_fee_btc();
        endpoints.send_to(
            ServiceBus::Sync,
            identity,
            self.bitcoin_syncer(),
            BusMsg::Sync(SyncMsg::Task(task)),
        )?;
        Ok(())
    }

    pub fn get_confs(&self, label: TxLabel) -> Option<u32> {
        self.confirmations.get(&label).copied().flatten()
    }
}
