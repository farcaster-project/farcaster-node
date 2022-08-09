use amplify::map;
use bitcoin::hashes::Hash;
use bitcoincore_rpc::RpcApi;
use clap::Parser;
use farcaster_core::blockchain::{Blockchain, Network};
use farcaster_node::syncerd::bitcoin_syncer::BitcoinSyncer;
use farcaster_node::syncerd::opts::Opts;
use farcaster_node::syncerd::runtime::SyncerdTask;
use farcaster_node::syncerd::types::{
    Abort, AddressAddendum, Boolean, BroadcastTransaction, BtcAddressAddendum, GetTx, SweepAddress,
    SweepAddressAddendum, Task, WatchAddress, WatchEstimateFee, WatchHeight, WatchTransaction,
};
use farcaster_node::syncerd::SweepBitcoinAddress;
use farcaster_node::syncerd::{runtime::Synclet, TaskId, TaskTarget};
use farcaster_node::ServiceId;
use microservices::ZMQ_CONTEXT;
use ntest::timeout;
use paste::paste;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;

use utils::assert;
use utils::config;
use utils::fc::bitcoin_setup;
use utils::misc;
use utils::setup_logging;

#[macro_use]
extern crate log;

mod utils;

const SOURCE1: ServiceId = ServiceId::Syncer(Blockchain::Bitcoin, Network::Local);

/*
These tests need to run serially, otherwise we cannot verify events based on the
state of electrum and bitcoin, for that we use `--test-threads=1` when running
`cargo test`

Timeout of 5 min max per test, otherwise test panic. This mitigate test that hangs
because of syncers.
*/

macro_rules! make_polling_test {
    ($name:ident) => {
        paste! {
            #[test]
            #[timeout(600000)]
            #[ignore]
            fn [< $name _polling >] () {
                $name(true);
            }

            #[test]
            #[timeout(600000)]
            #[ignore]
            fn [< $name _no_polling >] () {
                $name(false);
            }
        }
    };
}

make_polling_test!(bitcoin_syncer_block_height_test);
make_polling_test!(bitcoin_syncer_address_test);
make_polling_test!(bitcoin_syncer_transaction_test);
make_polling_test!(bitcoin_syncer_broadcast_tx_test);

#[test]
#[timeout(600000)]
#[ignore]
fn bitcoin_syncer_retrieve_transaction_test() {
    setup_logging();
    let bitcoin_rpc = bitcoin_setup();
    let (tx, rx_event) = create_bitcoin_syncer(true, "gettransaction");

    let address = bitcoin_rpc.get_new_address(None, None).unwrap();

    let txid = bitcoin_rpc
        .send_to_address(
            &address,
            bitcoin::Amount::from_sat(500),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);

    let task = SyncerdTask {
        task: Task::GetTx(GetTx {
            id: TaskId(1),
            hash: txid.to_vec(),
        }),
        source: SOURCE1.clone(),
    };
    tx.send(task).unwrap();
    let message = rx_event.recv_multipart(0).unwrap();
    let request = misc::get_request_from_message(message);
    info!("received request: {:?}", request);
    assert::transaction_received(request, txid);
}

#[test]
#[timeout(300000)]
#[ignore]
fn bitcoin_syncer_estimate_fee_test() {
    bitcoin_setup();
    let (tx, rx_event) = create_bitcoin_syncer(true, "estimatefee");

    let task = SyncerdTask {
        task: Task::WatchEstimateFee(WatchEstimateFee {
            id: TaskId(1),
            lifetime: 0,
        }),
        source: SOURCE1.clone(),
    };
    tx.send(task).unwrap();
    let message = rx_event.recv_multipart(0).unwrap();
    let request = misc::get_request_from_message(message);
    assert::fee_estimation_received(request);

    tx.send(SyncerdTask {
        task: Task::Terminate,
        source: SOURCE1.clone(),
    })
    .unwrap();
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);
}

/*
We test for the following scenarios in the block height tests:

- Submit a WatchHeight task, and immediately receive a HeightChanged event

- Mine a block and receive a single HeightChanged event

- Submit another WatchHeigh task,and immediately receive a HeightChanged event

- Mine another block and receive two HeightChanged events
*/
fn bitcoin_syncer_block_height_test(polling: bool) {
    setup_logging();
    let bitcoin_rpc = bitcoin_setup();
    let (tx, rx_event) = create_bitcoin_syncer(polling, "block_height");

    let address = bitcoin_rpc.get_new_address(None, None).unwrap();
    let blocks = bitcoin_rpc.get_block_count().unwrap();

    // Send a WatchHeight task
    let task = SyncerdTask {
        task: Task::WatchHeight(WatchHeight {
            id: TaskId(0),
            lifetime: blocks + 2,
        }),
        source: SOURCE1.clone(),
    };
    tx.send(task).unwrap();

    // Receive the request and compare it to the actual block count
    info!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("height changed");
    let request = misc::get_request_from_message(message);
    let blocks = bitcoin_rpc.get_block_count().unwrap();
    assert::received_height_changed(request, blocks);
    // Generate a single height changed event
    bitcoin_rpc.generate_to_address(1, &address).unwrap();
    info!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("height changed");
    let request = misc::get_request_from_message(message);
    let blocks = bitcoin_rpc.get_block_count().unwrap();
    assert::received_height_changed(request, blocks);

    // Send another WatchHeight task
    let task = SyncerdTask {
        task: Task::WatchHeight(WatchHeight {
            id: TaskId(1),
            lifetime: blocks + 2,
        }),
        source: SOURCE1.clone(),
    };
    tx.send(task).unwrap();
    let message = rx_event.recv_multipart(0).unwrap();
    let request = misc::get_request_from_message(message);
    assert::received_height_changed(request, blocks);

    // generate another block - this should result in two height changed messages
    bitcoin_rpc.generate_to_address(1, &address).unwrap();
    info!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("height changed");
    let request = misc::get_request_from_message(message);
    let blocks = bitcoin_rpc.get_block_count().unwrap();
    assert::received_height_changed(request, blocks);
    info!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("height changed");
    let request = misc::get_request_from_message(message);
    let blocks = bitcoin_rpc.get_block_count().unwrap();
    assert::received_height_changed(request, blocks);

    tx.send(SyncerdTask {
        task: Task::Terminate,
        source: SOURCE1.clone(),
    })
    .unwrap();
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);
}

/*
We test for the following scenarios in the address transaction tests:

- Submit a WatchAddress task with an address with no history yet, then create a
transaction for it and check the respective event

- Create a coinbase transaction to the same address and check the respective event

- Submit a WatchAddress task with another address in parallel, then create two
transactions for it and check for both respective events

- Submit a WatchAddress task with the same address again, observe if it receives
the complete existing transaction history

- Submit a WatchAddress task many times with the same address, ensure we receive
many times the same event

- Submit a WatchAddress task with a minimum height to an address with a
transaction below said height, ensure we receive only a later transaction above
the minimum height

*/
fn bitcoin_syncer_address_test(polling: bool) {
    setup_logging();
    let bitcoin_rpc = bitcoin_setup();
    let (tx, rx_event) = create_bitcoin_syncer(polling, "address");

    // generate some blocks to an address
    let address = bitcoin_rpc.get_new_address(None, None).unwrap();
    // 294 Satoshi is the dust limit for a segwit transaction
    let amount = bitcoin::Amount::ONE_SAT * 294;

    let blocks = bitcoin_rpc.get_block_count().unwrap();

    // Generate two addresses and watch them
    let address1 = bitcoin_rpc.get_new_address(None, None).unwrap();
    let address2 = bitcoin_rpc.get_new_address(None, None).unwrap();

    let addendum_1 = AddressAddendum::Bitcoin(BtcAddressAddendum {
        from_height: 0,
        address: address1.clone(),
    });
    let addendum_2 = AddressAddendum::Bitcoin(BtcAddressAddendum {
        from_height: 0,
        address: address2.clone(),
    });
    let watch_address_task_1 = SyncerdTask {
        task: Task::WatchAddress(WatchAddress {
            id: TaskId(1),
            lifetime: blocks + 1,
            addendum: addendum_1,
            include_tx: Boolean::True,
        }),
        source: SOURCE1.clone(),
    };
    tx.send(watch_address_task_1).unwrap();
    let watch_address_task_2 = SyncerdTask {
        task: Task::WatchAddress(WatchAddress {
            id: TaskId(1),
            lifetime: blocks + 2,
            addendum: addendum_2.clone(),
            include_tx: Boolean::True,
        }),
        source: SOURCE1.clone(),
    };
    tx.send(watch_address_task_2).unwrap();

    // send some coins to address1
    let txid = bitcoin_rpc
        .send_to_address(&address1, amount, None, None, None, None, None, None)
        .unwrap();
    info!("waiting for address transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received address transaction message");
    let request = misc::get_request_from_message(message);
    assert::address_transaction(request, amount.as_sat(), vec![txid.to_vec()]);

    // now generate a block for address1, then wait for the response and test it
    let block_hash = bitcoin_rpc.generate_to_address(1, &address1).unwrap();
    info!("waiting for address transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received address transaction message");
    let request = misc::get_request_from_message(message);
    let block = bitcoin_rpc.get_block(&block_hash[0]).unwrap();
    let address_transaction_amount = find_coinbase_transaction_amount(block.txdata.clone());
    let address_txid = find_coinbase_transaction_id(block.txdata);
    assert::address_transaction(
        request,
        address_transaction_amount,
        vec![address_txid.to_vec()],
    );

    // then send a transaction to the other address we are watching
    let txid_1 = bitcoin_rpc
        .send_to_address(&address2, amount, None, None, None, None, None, None)
        .unwrap();
    let txid_2 = bitcoin_rpc
        .send_to_address(&address2, amount, None, None, None, None, None, None)
        .unwrap();
    info!("waiting for address transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received address transaction message");
    let request = misc::get_request_from_message(message);
    assert::address_transaction(
        request,
        amount.as_sat(),
        vec![txid_1.to_vec(), txid_2.to_vec()],
    );

    info!("waiting for address transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received address transaction message");
    let request = misc::get_request_from_message(message);
    assert::address_transaction(
        request,
        amount.as_sat(),
        vec![txid_1.to_vec(), txid_2.to_vec()],
    );

    // watch for the same address, it should already contain transactions
    let watch_address_task_3 = SyncerdTask {
        task: Task::WatchAddress(WatchAddress {
            id: TaskId(1),
            lifetime: blocks + 2,
            addendum: addendum_2,
            include_tx: Boolean::True,
        }),
        source: SOURCE1.clone(),
    };
    tx.send(watch_address_task_3).unwrap();
    info!("waiting for address transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received address transaction message");
    let request = misc::get_request_from_message(message);
    assert::address_transaction(
        request,
        amount.as_sat(),
        vec![txid_1.to_vec(), txid_2.to_vec()],
    );
    info!("waiting for address transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received address transaction message");
    let request = misc::get_request_from_message(message);
    assert::address_transaction(
        request,
        amount.as_sat(),
        vec![txid_1.to_vec(), txid_2.to_vec()],
    );

    let address4 = bitcoin_rpc.get_new_address(None, None).unwrap();
    let addendum_4 = AddressAddendum::Bitcoin(BtcAddressAddendum {
        from_height: 0,
        address: address4.clone(),
    });
    for i in 0..5 {
        tx.send(SyncerdTask {
            task: Task::WatchAddress(WatchAddress {
                id: TaskId(i),
                lifetime: blocks + 5,
                addendum: addendum_4.clone(),
                include_tx: Boolean::True,
            }),
            source: SOURCE1.clone(),
        })
        .unwrap();
    }
    let txid = bitcoin_rpc
        .send_to_address(&address4, amount, None, None, None, None, None, None)
        .unwrap();

    for _ in 0..5 {
        info!("waiting for repeated address transaction message");
        let message = rx_event.recv_multipart(0).unwrap();
        info!("received repeated address transaction message");
        let request = misc::get_request_from_message(message);
        assert::address_transaction(request, amount.as_sat(), vec![txid.to_vec()]);
    }

    let address5 = bitcoin_rpc.get_new_address(None, None).unwrap();
    bitcoin_rpc
        .send_to_address(&address5, amount, None, None, None, None, None, None)
        .unwrap();
    bitcoin_rpc.generate_to_address(1, &address).unwrap();
    let blocks = bitcoin_rpc.get_block_count().unwrap();

    let addendum_5 = AddressAddendum::Bitcoin(BtcAddressAddendum {
        from_height: blocks,
        address: address5.clone(),
    });
    tx.send(SyncerdTask {
        task: Task::WatchAddress(WatchAddress {
            id: TaskId(5),
            lifetime: blocks + 5,
            addendum: addendum_5,
            include_tx: Boolean::False,
        }),
        source: SOURCE1.clone(),
    })
    .unwrap();
    let txid = bitcoin_rpc
        .send_to_address(&address5, amount, None, None, None, None, None, None)
        .unwrap();
    bitcoin_rpc.generate_to_address(1, &address).unwrap();
    info!("waiting for address transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received address transaction message");
    let request = misc::get_request_from_message(message);
    assert::address_transaction(request, amount.as_sat(), vec![txid.to_vec()]);

    tx.send(SyncerdTask {
        task: Task::Terminate,
        source: SOURCE1.clone(),
    })
    .unwrap();
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);
}

/*
We test for the following scenarios in the transaction tests:

- Submit a WatchTransaction task for a transaction in the mempool, but with a
confirmation bound of 0. Receive a single confirmation event.

- Submit a WatchTransaction task for a transaction in the mempool, receive confirmation events until
the threshold confs are reached

- Submit a WatchTransaction task for a mined transaction, receive confirmation events

- Submit two WatchTransaction tasks in parallel with the same recipient address, receive confirmation events for both
*/
fn bitcoin_syncer_transaction_test(polling: bool) {
    setup_logging();
    info!("logging set up");
    let bitcoin_rpc = bitcoin_setup();
    info!("bitcoin set up");
    let (tx, rx_event) = create_bitcoin_syncer(polling, "transaction");
    info!("syncer created");

    // generate some blocks to an address
    let reusable_address = bitcoin_rpc.get_new_address(None, None).unwrap();
    // 294 Satoshi is the dust limit for a segwit transaction
    let amount = bitcoin::Amount::ONE_SAT * 294;

    let address_1 = bitcoin_rpc.get_new_address(None, None).unwrap();
    let blocks = bitcoin_rpc.get_block_count().unwrap();
    let txid_1 = bitcoin_rpc
        .send_to_address(&address_1, amount, None, None, None, None, None, None)
        .unwrap();

    info!("sleeping to index the mempool transaction");
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);
    info!("sleep to index the mempool transaction is over");

    tx.send(SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: TaskId(1),
            lifetime: blocks + 5,
            hash: txid_1.to_vec(),
            confirmation_bound: 0,
        }),
        source: SOURCE1.clone(),
    })
    .unwrap();
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(0), vec![0]);

    tx.send(SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: TaskId(1),
            lifetime: blocks + 5,
            hash: txid_1.to_vec(),
            confirmation_bound: 2,
        }),
        source: SOURCE1.clone(),
    })
    .unwrap();

    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(0), vec![0]);

    let block_hash = bitcoin_rpc
        .generate_to_address(1, &reusable_address)
        .unwrap();
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(1), block_hash[0].to_vec());

    bitcoin_rpc
        .generate_to_address(1, &reusable_address)
        .unwrap();
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(2), block_hash[0].to_vec());

    let address_2 = bitcoin_rpc.get_new_address(None, None).unwrap();
    let block_hash = bitcoin_rpc.generate_to_address(1, &address_2).unwrap();
    let block = bitcoin_rpc.get_block(&block_hash[0]).unwrap();
    let address_txid = find_coinbase_transaction_id(block.txdata);

    std::thread::sleep(duration);

    tx.send(SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: TaskId(1),
            lifetime: blocks + 5,
            hash: address_txid.to_vec(),
            confirmation_bound: 2,
        }),
        source: SOURCE1.clone(),
    })
    .unwrap();

    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(1), block_hash[0].to_vec());

    bitcoin_rpc
        .generate_to_address(1, &reusable_address)
        .unwrap();
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(2), block_hash[0].to_vec());

    let address_3 = bitcoin_rpc.get_new_address(None, None).unwrap();
    let txid_2 = bitcoin_rpc
        .send_to_address(&address_3, amount, None, None, None, None, None, None)
        .unwrap();
    let txid_3 = bitcoin_rpc
        .send_to_address(&address_3, amount, None, None, None, None, None, None)
        .unwrap();

    std::thread::sleep(duration);

    tx.send(SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: TaskId(1),
            lifetime: blocks + 5,
            hash: txid_2.to_vec(),
            confirmation_bound: 2,
        }),
        source: SOURCE1.clone(),
    })
    .unwrap();
    tx.send(SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: TaskId(1),
            lifetime: blocks + 5,
            hash: txid_3.to_vec(),
            confirmation_bound: 2,
        }),
        source: SOURCE1.clone(),
    })
    .unwrap();

    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(0), vec![0]);
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(0), vec![0]);

    let address_4 = bitcoin_rpc.get_new_address(None, None).unwrap();

    let utxos = bitcoin_rpc
        .list_unspent(Some(100), None, None, Some(false), None)
        .unwrap();
    let bitcoincore_rpc::bitcoincore_rpc_json::ListUnspentResultEntry {
        txid,
        vout,
        amount: in_amount,
        ..
    } = utxos.iter().max_by_key(|utxo| utxo.amount).unwrap();

    let out_amount = *in_amount - amount - amount;
    let transaction = bitcoin_rpc
        .create_raw_transaction_hex(
            &[
                bitcoincore_rpc::bitcoincore_rpc_json::CreateRawTransactionInput {
                    txid: *txid,
                    vout: *vout,
                    sequence: None,
                },
            ],
            &map! {address_4.to_string() => out_amount},
            None,
            None,
        )
        .unwrap();
    let signed_tx = bitcoin_rpc
        .sign_raw_transaction_with_wallet(transaction, None, None)
        .unwrap();

    let txid = signed_tx.transaction().unwrap().txid();

    tx.send(SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: TaskId(1),
            lifetime: blocks + 5,
            hash: txid.to_vec(),
            confirmation_bound: 2,
        }),
        source: SOURCE1.clone(),
    })
    .unwrap();

    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, None, vec![0]);

    info!("sending raw transaction");
    bitcoin_rpc
        .send_raw_transaction(&signed_tx.transaction().unwrap())
        .unwrap();

    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(0), vec![0]);

    tx.send(SyncerdTask {
        task: Task::Terminate,
        source: SOURCE1.clone(),
    })
    .unwrap();
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);
}

/*
We test for the following scenarios in the abort tests:

- Submit a WatchTransaction task for a non-existing transaction, receive a confirmation events

- Submit an Abort task for the transaction, receive success

- Submit an Abort task for the transaction, receive an error

- Submit two WatchTransaction tasks, abort them both and receive both their aborted id's.
*/
#[test]
#[timeout(300000)]
#[ignore]
fn bitcoin_syncer_abort_test() {
    setup_logging();
    let bitcoin_rpc = bitcoin_setup();
    let (tx, rx_event) = create_bitcoin_syncer(true, "abort");

    let blocks = bitcoin_rpc.get_block_count().unwrap();

    let task = SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: TaskId(0),
            lifetime: blocks + 10,
            hash: vec![0; 32],
            confirmation_bound: 2,
        }),
        source: SOURCE1.clone(),
    };
    tx.send(task).unwrap();
    info!("waiting for confirmation");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, None, vec![0]);

    let task = SyncerdTask {
        task: Task::Abort(Abort {
            task_target: TaskTarget::TaskId(TaskId(0)),
            respond: Boolean::True,
        }),
        source: SOURCE1.clone(),
    };
    tx.send(task).unwrap();
    info!("waiting for task aborted message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("task aborted");
    let request = misc::get_request_from_message(message);
    assert::task_aborted(request, None, vec![0]);

    let task = SyncerdTask {
        task: Task::Abort(Abort {
            task_target: TaskTarget::TaskId(TaskId(0)),
            respond: Boolean::True,
        }),
        source: SOURCE1.clone(),
    };
    tx.send(task).unwrap();
    info!("waiting for task aborted message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("task aborted");
    let request = misc::get_request_from_message(message);
    assert::task_aborted(
        request,
        Some("abort failed, task from source Bitcoin (Local) syncer not found".to_string()),
        vec![],
    );

    let task = SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: TaskId(0),
            lifetime: blocks + 10,
            hash: vec![0; 32],
            confirmation_bound: 2,
        }),
        source: SOURCE1.clone(),
    };
    tx.send(task).unwrap();
    info!("waiting for confirmation");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, None, vec![0]);
    let task = SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: TaskId(1),
            lifetime: blocks + 10,
            hash: vec![0; 32],
            confirmation_bound: 2,
        }),
        source: SOURCE1.clone(),
    };
    tx.send(task).unwrap();
    info!("waiting for confirmation");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, None, vec![0]);
    let task = SyncerdTask {
        task: Task::Abort(Abort {
            task_target: TaskTarget::AllTasks,
            respond: Boolean::True,
        }),
        source: SOURCE1.clone(),
    };
    tx.send(task).unwrap();
    info!("waiting for task aborted message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("task aborted");
    let request = misc::get_request_from_message(message);
    assert::task_aborted(request, None, vec![0, 1]);

    let task = SyncerdTask {
        task: Task::Abort(Abort {
            task_target: TaskTarget::TaskId(TaskId(0)),
            respond: Boolean::True,
        }),
        source: SOURCE1.clone(),
    };
    tx.send(task).unwrap();
    info!("waiting for task aborted message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("task aborted");
    let request = misc::get_request_from_message(message);
    assert::task_aborted(
        request,
        Some("abort failed, task from source Bitcoin (Local) syncer not found".to_string()),
        vec![],
    );
    tx.send(SyncerdTask {
        task: Task::Terminate,
        source: SOURCE1.clone(),
    })
    .unwrap();
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);
}

/*
We test the following scenarios in the broadcast tx tests:

- Submit a BroadcastTransaction task, receive an event with a double spend error

- Submit a BroadcastTransaction task, receive a success event
*/
fn bitcoin_syncer_broadcast_tx_test(polling: bool) {
    setup_logging();
    let bitcoin_rpc = bitcoin_setup();
    let (tx, rx_event) = create_bitcoin_syncer(polling, "broadcast");

    let address = bitcoin_rpc.get_new_address(None, None).unwrap();

    // 294 Satoshi is the dust limit for a segwit transaction
    let amount = bitcoin::Amount::ONE_SAT * 294;
    // send some coins to address1
    let txid = bitcoin_rpc
        .send_to_address(&address, amount, None, None, None, None, None, None)
        .unwrap();
    let transaction = bitcoin_rpc.get_transaction(&txid, None).unwrap();

    // Generate over 101 blocks to reach block maturity, and some more for extra
    // leeway
    bitcoin_rpc.generate_to_address(110, &address).unwrap();

    // allow some time for things to happen, like the electrum server catching up
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);

    let task = SyncerdTask {
        task: Task::BroadcastTransaction(BroadcastTransaction {
            id: TaskId(0),
            tx: transaction.hex,
        }),
        source: SOURCE1.clone(),
    };
    tx.send(task).unwrap();

    info!("waiting for transaction broadcasted message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("transaction broadcasted");
    let request = misc::get_request_from_message(message);
    assert::transaction_broadcasted(request, true, None);

    let utxos = bitcoin_rpc
        .list_unspent(Some(100), None, None, Some(false), None)
        .unwrap();
    let bitcoincore_rpc::bitcoincore_rpc_json::ListUnspentResultEntry {
        txid,
        vout,
        amount: in_amount,
        ..
    } = utxos.iter().max_by_key(|utxo| utxo.amount).unwrap();

    let out_amount = *in_amount - amount - amount;
    let transaction = bitcoin_rpc
        .create_raw_transaction_hex(
            &[
                bitcoincore_rpc::bitcoincore_rpc_json::CreateRawTransactionInput {
                    txid: *txid,
                    vout: *vout,
                    sequence: None,
                },
            ],
            &map! {address.to_string() => out_amount},
            None,
            None,
        )
        .unwrap();
    let signed_tx = bitcoin_rpc
        .sign_raw_transaction_with_wallet(transaction, None, None)
        .unwrap();
    let task = SyncerdTask {
        task: Task::BroadcastTransaction(BroadcastTransaction {
            id: TaskId(0),
            tx: signed_tx.hex,
        }),
        source: SOURCE1.clone(),
    };
    tx.send(task).unwrap();

    info!("waiting for transaction broadcasted message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("transaction broadcasted");
    let request = misc::get_request_from_message(message);
    assert::transaction_broadcasted(request, false, None);
    tx.send(SyncerdTask {
        task: Task::Terminate,
        source: SOURCE1.clone(),
    })
    .unwrap();
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);
}

/*
We test the following scenarios in the broadcast tx tests:

- Submit a sweep address task sweeping an address owning a single output,
receive a success event and check that the address balance is sweeped

- Submit a sweep address task sweeping an address owning two outputs,
receive a success event and check that the address balance is sweeped

*/
#[test]
#[timeout(600000)]
#[ignore]
fn bitcoin_syncer_sweep_address_test() {
    setup_logging();
    let bitcoin_rpc = bitcoin_setup();
    let (tx, rx_event) = create_bitcoin_syncer(false, "sweep");

    let reusable_address = bitcoin_rpc.get_new_address(None, None).unwrap();
    let sweep_source_address = bitcoin_rpc.get_new_address(None, None).unwrap();
    let sweep_destination_address_1 = bitcoin_rpc.get_new_address(None, None).unwrap();
    let sweep_destination_address_2 = bitcoin_rpc.get_new_address(None, None).unwrap();
    let source_secret_key = bitcoin_rpc
        .dump_private_key(&sweep_source_address)
        .unwrap()
        .inner;

    // 294 Satoshi is the dust limit for a segwit transaction
    let amount = bitcoin::Amount::ONE_SAT * 1000;
    // send some coins to sweep_source_address
    let _txid = bitcoin_rpc
        .send_to_address(
            &sweep_source_address,
            amount,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

    let blocks = bitcoin_rpc.get_block_count().unwrap();
    // allow some time for things to happen, like the electrum server catching up
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);

    let task = SyncerdTask {
        task: Task::SweepAddress(SweepAddress {
            id: TaskId(0),
            lifetime: blocks,
            from_height: None,
            retry: true,
            addendum: SweepAddressAddendum::Bitcoin(SweepBitcoinAddress {
                source_secret_key,
                source_address: sweep_source_address.clone(),
                destination_address: sweep_destination_address_1.clone(),
            }),
        }),
        source: SOURCE1.clone(),
    };
    tx.send(task).unwrap();
    // allow some time for things to happen, like the electrum server catching up
    info!("waiting for address sweeped message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("address sweeped");
    let _request = misc::get_request_from_message(message);
    bitcoin_rpc
        .generate_to_address(1, &reusable_address)
        .unwrap();

    let balance_1 = bitcoin_rpc
        .get_received_by_address(&sweep_destination_address_1, None)
        .unwrap();
    info!("received balance: {:?}", balance_1);
    assert!(balance_1.as_sat() > 0);

    // send some coins to sweep_source_address
    let _txid = bitcoin_rpc
        .send_to_address(
            &sweep_source_address,
            amount,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    // send some coins to sweep_source_address
    let _txid = bitcoin_rpc
        .send_to_address(
            &sweep_source_address,
            amount,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

    let blocks = bitcoin_rpc.get_block_count().unwrap();
    // allow some time for things to happen, like the electrum server catching up
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);

    let task = SyncerdTask {
        task: Task::SweepAddress(SweepAddress {
            id: TaskId(0),
            lifetime: blocks,
            from_height: None,
            retry: true,
            addendum: SweepAddressAddendum::Bitcoin(SweepBitcoinAddress {
                source_secret_key,
                source_address: sweep_source_address,
                destination_address: sweep_destination_address_2.clone(),
            }),
        }),
        source: SOURCE1.clone(),
    };
    tx.send(task).unwrap();
    // allow some time for things to happen, like the electrum server catching up
    info!("waiting for address sweeped message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("address sweeped");
    let _request = misc::get_request_from_message(message);
    bitcoin_rpc
        .generate_to_address(1, &reusable_address)
        .unwrap();

    let balance_2 = bitcoin_rpc
        .get_received_by_address(&sweep_destination_address_2, None)
        .unwrap();
    info!("received balance: {:?}", balance_2);
    assert!(balance_2.as_sat() > balance_1.as_sat());
    tx.send(SyncerdTask {
        task: Task::Terminate,
        source: SOURCE1.clone(),
    })
    .unwrap();
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);
}

// =========================
// TODO: move into utils from here
//

fn create_bitcoin_syncer(
    polling: bool,
    socket_name: &str,
) -> (std::sync::mpsc::Sender<SyncerdTask>, zmq::Socket) {
    use rand::prelude::*;
    let mut rng = rand::thread_rng();
    let id: u64 = rng.gen();
    let addr = format!("inproc://testbitcoinbridge-{}-{}", socket_name, id);
    debug!("creating Bitcoin syncer on addr {}", addr);

    let (tx, rx): (Sender<SyncerdTask>, Receiver<SyncerdTask>) = std::sync::mpsc::channel();
    let tx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
    let rx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
    tx_event.connect(&addr).unwrap();
    rx_event.bind(&addr).unwrap();
    let mut syncer = BitcoinSyncer::new();

    let conf = config::TestConfig::parse();
    let opts = Opts::parse_from(vec!["syncerd"].into_iter().chain(vec![
        "--blockchain",
        "Bitcoin",
        "--electrum-server",
        &format!("{}", conf.electrs),
    ]));

    syncer
        .run(
            rx,
            tx_event,
            SOURCE1.clone().into(),
            &opts,
            Network::Local,
            polling,
        )
        .expect("Invalid Bitcoin syncer!");
    (tx, rx_event)
}

fn find_coinbase_transaction_id(txs: Vec<bitcoin::Transaction>) -> bitcoin::Txid {
    for transaction in txs {
        if transaction.input[0].previous_output.txid == bitcoin::Txid::from_slice(&[0; 32]).unwrap()
        {
            return transaction.txid();
        }
    }
    bitcoin::Txid::from_slice(&[0; 32]).unwrap()
}

fn find_coinbase_transaction_amount(txs: Vec<bitcoin::Transaction>) -> u64 {
    for transaction in txs {
        if transaction.input[0].previous_output.txid == bitcoin::Txid::from_slice(&[0; 32]).unwrap()
        {
            return transaction.output[0].value;
        }
    }
    0
}
