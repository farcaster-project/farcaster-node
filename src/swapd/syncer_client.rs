use crate::{
    service::LogStyle,
    syncerd::{
        AddressAddendum, Boolean, BtcAddressAddendum, Coin, GetTx, SweepAddress,
        SweepAddressAddendum, SweepXmrAddress, WatchAddress, WatchTransaction, XmrAddressAddendum,
    },
};
use bitcoin::{Script, Txid};
use farcaster_core::{swap::SwapId, transaction::TxLabel};
use std::collections::HashMap;

use crate::{
    rpc::Request,
    syncerd::{Task, TaskId},
    ServiceId,
};
use strict_encoding::{StrictDecode, StrictEncode};

pub struct SyncerTasks {
    pub counter: u32,
    pub watched_txs: HashMap<TaskId, TxLabel>,
    pub final_txs: HashMap<TxLabel, bool>,
    pub watched_addrs: HashMap<TaskId, TxLabel>,
    pub retrieving_txs: HashMap<TaskId, (TxLabel, Task)>,
    pub sweeping_addr: Option<TaskId>,
    // external address: needed to subscribe for buy (bob) or refund (alice) address_txs
    pub txids: HashMap<TxLabel, Txid>,
}

impl SyncerTasks {
    pub fn new_taskid(&mut self) -> TaskId {
        self.counter += 1;
        TaskId(self.counter)
    }
}

pub struct SyncerState {
    pub swap_id: SwapId,
    pub tasks: SyncerTasks,
    pub bitcoin_height: u64,
    pub monero_height: u64,
    pub confirmation_bound: u32,
    pub lock_tx_confs: Option<Request>,
    pub cancel_tx_confs: Option<Request>,
    pub network: farcaster_core::blockchain::Network,
    pub bitcoin_syncer: ServiceId,
    pub monero_syncer: ServiceId,
    pub monero_amount: monero::Amount,
    pub bitcoin_amount: bitcoin::Amount,
    pub awaiting_funding: bool,
}
impl SyncerState {
    pub fn task_lifetime(&self, coin: Coin) -> u64 {
        let height = self.height(coin);
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
    pub fn height(&self, coin: Coin) -> u64 {
        match coin {
            Coin::Bitcoin => self.bitcoin_height,
            Coin::Monero => self.monero_height,
        }
    }
    pub fn handle_height_change(&mut self, new_height: u64, coin: Coin) {
        let height = match coin {
            Coin::Bitcoin => &mut self.bitcoin_height,
            Coin::Monero => &mut self.monero_height,
        };
        if &new_height > height {
            debug!("{} new height {}", coin, &new_height);
            *height = new_height;
        } else {
            warn!("block height did not increment, maybe syncer sends multiple events");
        }
    }
    pub fn watch_tx_btc(&mut self, txid: Txid, tx_label: TxLabel) -> Task {
        let id = self.tasks.new_taskid();
        self.tasks.watched_txs.insert(id, tx_label);
        info!(
            "{} | Watching {} transaction ({})",
            self.swap_id.bright_blue_italic(),
            tx_label.bright_white_bold(),
            txid.bright_yellow_italic()
        );
        Task::WatchTransaction(WatchTransaction {
            id,
            lifetime: self.task_lifetime(Coin::Bitcoin),
            hash: txid.to_vec(),
            confirmation_bound: self.confirmation_bound,
        })
    }
    pub fn is_watched_tx(&self, tx_label: &TxLabel) -> bool {
        self.tasks.watched_txs.values().any(|tx| tx == tx_label)
    }
    pub fn watch_tx_xmr(&mut self, hash: Vec<u8>, tx_label: TxLabel) -> Task {
        let id = self.tasks.new_taskid();
        self.tasks.watched_txs.insert(id, tx_label);
        info!(
            "{} | Watching {} transaction ({})",
            self.swap_id.bright_blue_italic(),
            tx_label.bright_white_bold(),
            hex::encode(&hash).bright_yellow_italic(),
        );
        debug!("Watching transaction {} with {}", hex::encode(&hash), id);
        Task::WatchTransaction(WatchTransaction {
            id,
            lifetime: self.task_lifetime(Coin::Monero),
            hash,
            confirmation_bound: self.confirmation_bound,
        })
    }
    pub fn retrieve_tx_btc(&mut self, txid: Txid, tx_label: TxLabel) -> Task {
        let id = self.tasks.new_taskid();
        let task = Task::GetTx(GetTx {
            id,
            hash: txid.to_vec(),
        });
        self.tasks
            .retrieving_txs
            .insert(id, (tx_label, task.clone()));
        task
    }
    pub fn watch_addr_btc(&mut self, script_pubkey: Script, tx_label: TxLabel) -> Task {
        let id = self.tasks.new_taskid();
        let from_height = self.from_height(Coin::Bitcoin, 6);
        self.tasks.watched_addrs.insert(id, tx_label);
        info!(
            "{} | Watching {} transaction with scriptPubkey: {}",
            self.swap_id.bright_blue_italic(),
            tx_label.bright_white_bold(),
            script_pubkey.asm().bright_blue_italic()
        );
        let addendum = BtcAddressAddendum {
            address: None,
            from_height,
            script_pubkey,
        };
        Task::WatchAddress(WatchAddress {
            id,
            lifetime: self.task_lifetime(Coin::Bitcoin),
            addendum: AddressAddendum::Bitcoin(addendum),
            include_tx: Boolean::True,
        })
    }

    pub fn is_watched_addr(&self, tx_label: &TxLabel) -> bool {
        self.tasks.watched_addrs.values().any(|tx| tx == tx_label)
    }
    pub fn from_height(&self, coin: Coin, delta: u64) -> u64 {
        let height = self.height(coin);
        let delta = if height > delta { delta } else { height };
        height - delta
    }

    pub fn watch_addr_xmr(
        &mut self,
        spend: monero::PublicKey,
        view: monero::PrivateKey,
        tx_label: TxLabel,
    ) -> Task {
        debug!(
            "{} | Address's secret view key for {}: {}",
            self.swap_id.bright_blue_italic(),
            tx_label.bright_white_bold(),
            view.bright_white_italic()
        );
        debug!(
            "{} | Address's public spend key for {}: {}",
            self.swap_id.bright_blue_italic(),
            tx_label.bright_white_bold(),
            spend.bright_white_italic()
        );
        let viewpair = monero::ViewPair { spend, view };
        let address = monero::Address::from_viewpair(self.network.into(), &viewpair);
        let from_height = self.from_height(Coin::Monero, 20);
        let addendum = AddressAddendum::Monero(XmrAddressAddendum {
            spend_key: spend,
            view_key: view,
            from_height,
        });

        let id = self.tasks.new_taskid();
        self.tasks.watched_addrs.insert(id, tx_label);

        info!(
            "{} | Watching {} on address {}",
            self.swap_id.bright_blue_italic(),
            tx_label.bright_white_bold(),
            address
        );

        let watch_addr = WatchAddress {
            id,
            lifetime: self.task_lifetime(Coin::Monero),
            addendum,
            include_tx: Boolean::False,
        };
        Task::WatchAddress(watch_addr)
    }
    pub fn sweep_xmr(
        &mut self,
        view_key: monero::PrivateKey,
        spend_key: monero::PrivateKey,
        address: monero::Address,
        from_height: Option<u64>,
        minimum_balance: monero::Amount,
    ) -> Task {
        let id = self.tasks.new_taskid();
        self.tasks.sweeping_addr = Some(id);
        let lifetime = self.task_lifetime(Coin::Monero);
        let addendum = SweepAddressAddendum::Monero(SweepXmrAddress {
            view_key,
            spend_key,
            address,
            minimum_balance,
        });
        let sweep_task = SweepAddress {
            id,
            lifetime,
            addendum,
            from_height,
        };
        Task::SweepAddress(sweep_task)
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
    ) {
        if let Some(txlabel) = self.tasks.watched_txs.get(id) {
            if !self.tasks.final_txs.contains_key(txlabel)
                && confirmations.is_some()
                && confirmations.unwrap() >= finality_thr
            {
                info!(
                    "{} | Tx {} {} with {} {}",
                    self.swap_id.bright_blue_italic(),
                    txlabel.bright_white_bold(),
                    "final".bright_green_bold(),
                    confirmations.unwrap().bright_green_bold(),
                    "confirmations".bright_green_bold()
                );
                self.tasks.final_txs.insert(*txlabel, true);
            } else if self.tasks.final_txs.contains_key(txlabel) {
                debug!(
                    "{} | Tx {} {} with {} {}",
                    self.swap_id.bright_blue_italic(),
                    txlabel.bright_white_bold(),
                    "final".bright_green_bold(),
                    confirmations.unwrap().bright_green_bold(),
                    "confirmations".bright_green_bold()
                );
            } else {
                match confirmations {
                    Some(0) => {
                        info!(
                            "{} | Tx {} on mempool but hasn't been mined",
                            swapid.bright_blue_italic(),
                            txlabel.bright_white_bold()
                        );
                    }
                    Some(confs) => {
                        info!(
                            "{} | Tx {} mined with {} {}",
                            swapid.bright_blue_italic(),
                            txlabel.bright_white_bold(),
                            confs.bright_green_bold(),
                            "confirmations".bright_green_bold(),
                        )
                    }
                    None => {
                        info!(
                            "{} | Tx {} not on the mempool",
                            swapid.bright_blue_italic(),
                            txlabel.bright_white_bold()
                        );
                    }
                }
            }
        } else {
            error!(
                "received event with unknown transaction and task id {}",
                &id
            )
        }
    }
}

pub fn log_tx_seen(swap_id: SwapId, txlabel: &TxLabel, txid: &Txid) {
    info!(
        "{} | {} transaction ({}) in mempool or blockchain, forward to walletd",
        swap_id.bright_blue_italic(),
        txlabel.bright_white_bold(),
        txid.bright_yellow_italic(),
    );
}

pub fn log_tx_received(swap_id: SwapId, txlabel: TxLabel) {
    info!(
        "{} | {} transaction received from {}",
        swap_id.bright_blue_italic(),
        txlabel.bright_white_bold(),
        ServiceId::Wallet
    );
}
