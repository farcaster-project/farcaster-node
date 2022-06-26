use amplify::map;
use bitcoincore_rpc::{Client, RpcApi};
use clap::Parser;
use farcaster_node::rpc::Request;
use farcaster_node::syncerd::bitcoin_syncer::BitcoinSyncer;
use farcaster_node::syncerd::monero_syncer::MoneroSyncer;
use farcaster_node::syncerd::opts::{Coin, Opts};
use farcaster_node::syncerd::runtime::SyncerdTask;
use farcaster_node::syncerd::SweepBitcoinAddress;
use farcaster_node::syncerd::{
    runtime::Synclet, SweepAddress, SweepAddressAddendum, SweepXmrAddress, TaskId, TaskTarget,
    XmrAddressAddendum,
};
use farcaster_node::ServiceId;
use internet2::transport::MAX_FRAME_SIZE;
use internet2::Decrypt;
use internet2::PlainTranscoder;
use internet2::RoutedFrame;
use internet2::ZMQ_CONTEXT;
use monero_rpc::GetBlockHeaderSelector;
use paste::paste;
use rand::{distributions::Alphanumeric, Rng};
use std::convert::TryInto;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;

use ntest::timeout;

use bitcoin::hashes::Hash;
use internet2::{CreateUnmarshaller, Unmarshall};
use std::str::FromStr;

use farcaster_core::blockchain::Network;
use farcaster_node::syncerd::types::{
    Abort, AddressAddendum, Boolean, BroadcastTransaction, BtcAddressAddendum, EstimateFee, Event,
    GetTx, Task, WatchAddress, WatchHeight, WatchTransaction,
};

use utils::config;

#[macro_use]
extern crate log;

mod utils;

const SOURCE1: ServiceId = ServiceId::Syncer(Coin::Bitcoin, Network::Local);
const SOURCE2: ServiceId = ServiceId::Syncer(Coin::Monero, Network::Local);

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
            #[timeout(300000)]
            #[ignore]
            fn [< $name _polling >] () {
                $name(true);
            }

            #[test]
            #[timeout(300000)]
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
    setup_logging(None);
    let bitcoin_rpc = bitcoin_setup();
    let address = bitcoin_rpc.get_new_address(None, None).unwrap();

    let (tx, rx_event) = create_bitcoin_syncer(true, "transaction");

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

    let duration = std::time::Duration::from_secs(30);
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
    let request = get_request_from_message(message);
    info!("received request: {:?}", request);
    assert_transaction_received(request, txid);
}

fn assert_transaction_received(request: Request, expected_txid: bitcoin::Txid) {
    match request {
        Request::SyncerdBridgeEvent(event) => match event.event {
            Event::TransactionRetrieved(transaction) => {
                assert_eq!(transaction.tx.unwrap().txid(), expected_txid);
            }
            _ => {
                panic!("expected height changed event");
            }
        },
        _ => {
            panic!("expected syncerd bridge event");
        }
    }
}

#[test]
#[timeout(300000)]
#[ignore]
fn bitcoin_syncer_estimate_fee_test() {
    bitcoin_setup();
    let (tx, rx_event) = create_bitcoin_syncer(true, "estimatefee");
    let task = SyncerdTask {
        task: Task::EstimateFee(EstimateFee {
            id: TaskId(1),
            blocks_until_confirmation: 1,
        }),
        source: SOURCE1.clone(),
    };
    tx.send(task).unwrap();
    let message = rx_event.recv_multipart(0).unwrap();
    let request = get_request_from_message(message);
    assert_fee_estimation_received(request)
}

fn assert_fee_estimation_received(request: Request) {
    match request {
        Request::SyncerdBridgeEvent(farcaster_node::rpc::request::SyncerdBridgeEvent {
            event: Event::FeeEstimation(fee_estimation),
            ..
        }) => {
            assert!(fee_estimation.btc_per_kbyte.is_some());
        }
        _ => {
            panic!("expected syncerd bridge event");
        }
    }
}

/*
We test for the following scenarios in the block height tests:

- Submit a WatchHeight task, and immediately receive a HeightChanged event

- Mine a block and receive a single HeightChanged event

- Submit another WatchHeigh task,and immediately receive a HeightChanged event

- Mine another block and receive two HeightChanged events
*/
fn bitcoin_syncer_block_height_test(polling: bool) {
    setup_logging(None);
    let bitcoin_rpc = bitcoin_setup();
    let address = bitcoin_rpc.get_new_address(None, None).unwrap();

    // start a bitcoin syncer
    let (tx, rx_event) = create_bitcoin_syncer(polling, "block_height");

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
    let request = get_request_from_message(message);
    let blocks = bitcoin_rpc.get_block_count().unwrap();
    assert_received_height_changed(request, blocks);
    // Generate a single height changed event
    bitcoin_rpc.generate_to_address(1, &address).unwrap();
    info!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("height changed");
    let request = get_request_from_message(message);
    let blocks = bitcoin_rpc.get_block_count().unwrap();
    assert_received_height_changed(request, blocks);

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
    let request = get_request_from_message(message);
    assert_received_height_changed(request, blocks);

    // generate another block - this should result in two height changed messages
    bitcoin_rpc.generate_to_address(1, &address).unwrap();
    info!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("height changed");
    let request = get_request_from_message(message);
    let blocks = bitcoin_rpc.get_block_count().unwrap();
    assert_received_height_changed(request, blocks);
    info!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("height changed");
    let request = get_request_from_message(message);
    let blocks = bitcoin_rpc.get_block_count().unwrap();
    assert_received_height_changed(request, blocks);
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
    setup_logging(None);
    let bitcoin_rpc = bitcoin_setup();

    // generate some blocks to an address
    let address = bitcoin_rpc.get_new_address(None, None).unwrap();
    // 294 Satoshi is the dust limit for a segwit transaction
    let amount = bitcoin::Amount::ONE_SAT * 294;
    // Generate over 101 blocks to reach block maturity, and some more for extra
    // leeway
    bitcoin_rpc.generate_to_address(110, &address).unwrap();

    // allow some time for things to happen, like the electrum server catching up
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);

    // start a bitcoin syncer
    let (tx, rx_event) = create_bitcoin_syncer(polling, "address");

    let blocks = bitcoin_rpc.get_block_count().unwrap();

    // Generate two addresses and watch them
    let address1 = bitcoin_rpc.get_new_address(None, None).unwrap();
    let address2 = bitcoin_rpc.get_new_address(None, None).unwrap();

    let addendum_1 = AddressAddendum::Bitcoin(BtcAddressAddendum {
        address: Some(address1.clone()),
        from_height: 0,
        script_pubkey: address1.script_pubkey(),
    });
    let addendum_2 = AddressAddendum::Bitcoin(BtcAddressAddendum {
        address: Some(address2.clone()),
        from_height: 0,
        script_pubkey: address2.script_pubkey(),
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
    let request = get_request_from_message(message);
    assert_address_transaction(request, amount.as_sat(), vec![txid.to_vec()]);

    // now generate a block for address1, then wait for the response and test it
    let block_hash = bitcoin_rpc.generate_to_address(1, &address1).unwrap();
    info!("waiting for address transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received address transaction message");
    let request = get_request_from_message(message);
    let block = bitcoin_rpc.get_block(&block_hash[0]).unwrap();
    let address_transaction_amount = find_coinbase_transaction_amount(block.txdata.clone());
    let address_txid = find_coinbase_transaction_id(block.txdata);
    assert_address_transaction(
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
    let request = get_request_from_message(message);
    assert_address_transaction(
        request,
        amount.as_sat(),
        vec![txid_1.to_vec(), txid_2.to_vec()],
    );

    info!("waiting for address transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received address transaction message");
    let request = get_request_from_message(message);
    assert_address_transaction(
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
    let request = get_request_from_message(message);
    assert_address_transaction(
        request,
        amount.as_sat(),
        vec![txid_1.to_vec(), txid_2.to_vec()],
    );
    info!("waiting for address transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received address transaction message");
    let request = get_request_from_message(message);
    assert_address_transaction(
        request,
        amount.as_sat(),
        vec![txid_1.to_vec(), txid_2.to_vec()],
    );

    let address4 = bitcoin_rpc.get_new_address(None, None).unwrap();
    let addendum_4 = AddressAddendum::Bitcoin(BtcAddressAddendum {
        address: Some(address4.clone()),
        from_height: 0,
        script_pubkey: address4.script_pubkey(),
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
        let request = get_request_from_message(message);
        assert_address_transaction(request, amount.as_sat(), vec![txid.to_vec()]);
    }

    let address5 = bitcoin_rpc.get_new_address(None, None).unwrap();
    bitcoin_rpc
        .send_to_address(&address5, amount, None, None, None, None, None, None)
        .unwrap();
    bitcoin_rpc.generate_to_address(1, &address).unwrap();
    let blocks = bitcoin_rpc.get_block_count().unwrap();

    let addendum_5 = AddressAddendum::Bitcoin(BtcAddressAddendum {
        address: Some(address5.clone()),
        from_height: blocks,
        script_pubkey: address5.script_pubkey(),
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
    let request = get_request_from_message(message);
    assert_address_transaction(request, amount.as_sat(), vec![txid.to_vec()]);
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
    setup_logging(None);
    let bitcoin_rpc = bitcoin_setup();

    // generate some blocks to an address
    let reusable_address = bitcoin_rpc.get_new_address(None, None).unwrap();
    bitcoin_rpc
        .generate_to_address(110, &reusable_address)
        .unwrap();

    // start a bitcoin syncer
    let (tx, rx_event) = create_bitcoin_syncer(polling, "transaction");

    // 294 Satoshi is the dust limit for a segwit transaction
    let amount = bitcoin::Amount::ONE_SAT * 294;

    // allow some time for things to happen, like the electrum server catching up
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);

    let address_1 = bitcoin_rpc.get_new_address(None, None).unwrap();
    let blocks = bitcoin_rpc.get_block_count().unwrap();
    let txid_1 = bitcoin_rpc
        .send_to_address(&address_1, amount, None, None, None, None, None, None)
        .unwrap();

    std::thread::sleep(duration);

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
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(0), vec![0]);

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
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(0), vec![0]);

    let block_hash = bitcoin_rpc
        .generate_to_address(1, &reusable_address)
        .unwrap();
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(1), block_hash[0].to_vec());

    bitcoin_rpc
        .generate_to_address(1, &reusable_address)
        .unwrap();
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(2), block_hash[0].to_vec());

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
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(1), block_hash[0].to_vec());

    bitcoin_rpc
        .generate_to_address(1, &reusable_address)
        .unwrap();
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(2), block_hash[0].to_vec());

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
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(0), vec![0]);
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(0), vec![0]);

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
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, None, vec![0]);

    info!("sending raw transaction");
    bitcoin_rpc
        .send_raw_transaction(&signed_tx.transaction().unwrap())
        .unwrap();

    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(0), vec![0]);
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
    setup_logging(None);
    let (tx, rx_event) = create_bitcoin_syncer(true, "abort");
    let bitcoin_rpc = bitcoin_setup();
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
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, None, vec![0]);

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
    let request = get_request_from_message(message);
    assert_task_aborted(request, None, vec![0]);

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
    let request = get_request_from_message(message);
    assert_task_aborted(
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
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, None, vec![0]);
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
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, None, vec![0]);
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
    let request = get_request_from_message(message);
    assert_task_aborted(request, None, vec![0, 1]);

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
    let request = get_request_from_message(message);
    assert_task_aborted(
        request,
        Some("abort failed, task from source Bitcoin (Local) syncer not found".to_string()),
        vec![],
    );
}

/*
We test the following scenarios in the broadcast tx tests:

- Submit a BroadcastTransaction task, receive an event with a double spend error

- Submit a BroadcastTransaction task, receive a success event
*/
fn bitcoin_syncer_broadcast_tx_test(polling: bool) {
    setup_logging(None);
    let bitcoin_rpc = bitcoin_setup();
    let address = bitcoin_rpc.get_new_address(None, None).unwrap();

    let (tx, rx_event) = create_bitcoin_syncer(polling, "broadcast");

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
    let request = get_request_from_message(message);
    assert_transaction_broadcasted(request, true, None);

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
    let request = get_request_from_message(message);
    assert_transaction_broadcasted(request, false, None);
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
    setup_logging(None);
    let bitcoin_rpc = bitcoin_setup();
    let reusable_address = bitcoin_rpc.get_new_address(None, None).unwrap();
    let sweep_source_address = bitcoin_rpc.get_new_address(None, None).unwrap();
    let sweep_destination_address_1 = bitcoin_rpc.get_new_address(None, None).unwrap();
    let sweep_destination_address_2 = bitcoin_rpc.get_new_address(None, None).unwrap();
    let wif_private_key = bitcoin_rpc.dump_private_key(&sweep_source_address).unwrap();

    let (tx, rx_event) = create_bitcoin_syncer(false, "broadcast");

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
            addendum: SweepAddressAddendum::Bitcoin(SweepBitcoinAddress {
                private_key: (&wif_private_key.to_bytes()[..]).try_into().unwrap(),
                address: sweep_source_address.clone(),
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
    let _request = get_request_from_message(message);
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
            addendum: SweepAddressAddendum::Bitcoin(SweepBitcoinAddress {
                private_key: (&wif_private_key.to_bytes()[..]).try_into().unwrap(),
                address: sweep_source_address,
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
    let _request = get_request_from_message(message);
    bitcoin_rpc
        .generate_to_address(1, &reusable_address)
        .unwrap();

    let balance_2 = bitcoin_rpc
        .get_received_by_address(&sweep_destination_address_2, None)
        .unwrap();
    info!("received balance: {:?}", balance_2);
    assert!(balance_2.as_sat() > balance_1.as_sat());
}

fn create_bitcoin_syncer(
    polling: bool,
    socket_name: &str,
) -> (std::sync::mpsc::Sender<SyncerdTask>, zmq::Socket) {
    let addr = format!("inproc://testmonerobridge-{}", socket_name);

    let (tx, rx): (Sender<SyncerdTask>, Receiver<SyncerdTask>) = std::sync::mpsc::channel();
    let tx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
    let rx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
    tx_event.connect(&addr).unwrap();
    rx_event.bind(&addr).unwrap();
    let mut syncer = BitcoinSyncer::new();

    let conf = config::TestConfig::parse();
    let opts = Opts::parse_from(vec!["syncerd"].into_iter().chain(vec![
        "--coin",
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
        .expect("Valid bitcoin syncer");
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

fn bitcoin_setup() -> bitcoincore_rpc::Client {
    let conf = config::TestConfig::parse();
    let bitcoin_rpc =
        Client::new(&format!("{}", conf.bitcoin.daemon), conf.bitcoin.get_auth()).unwrap();

    // make sure a wallet is created and loaded
    if bitcoin_rpc
        .create_wallet("wallet", None, None, None, None)
        .is_err()
    {
        let _ = bitcoin_rpc.load_wallet("wallet");
    }
    bitcoin_rpc
}

/*
We test for the following scenarios in the block height tests:

- Submit a WatchHeight task, and immediately receive a HeightChanged event

- Mine a block and receive a single HeightChanged event

- Submit another WatchHeigh task,and immediately receive a HeightChanged event

- Mine another block and receive two HeightChanged events
*/
#[tokio::test]
#[timeout(300000)]
#[ignore]
async fn monero_syncer_block_height_test() {
    setup_logging(None);
    let (regtest, wallet) = setup_monero().await;
    let address = wallet.get_address(0, None).await.unwrap();
    let blocks = regtest.generate_blocks(1, address.address).await.unwrap();

    // allow some time for things to happen, like the wallet server catching up
    let duration = std::time::Duration::from_secs(1);
    std::thread::sleep(duration);

    // create a monero syncer
    let (tx, rx_event) = create_monero_syncer("block_height", false);

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
    let request = get_request_from_message(message);
    assert_received_height_changed(request, blocks);
    // Generate a single height changed event
    let blocks = regtest.generate_blocks(1, address.address).await.unwrap();
    info!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("height changed");
    let request = get_request_from_message(message);
    // let blocks = regtest.get_block_count().await.unwrap();
    assert_received_height_changed(request, blocks);

    // Send another WatchHeight task
    let task = SyncerdTask {
        task: Task::WatchHeight(WatchHeight {
            id: TaskId(1),
            lifetime: blocks + 2,
        }),
        source: SOURCE1.clone(),
    };
    tx.send(task).unwrap();
    info!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("height changed");
    let request = get_request_from_message(message);
    assert_received_height_changed(request, blocks);

    // generate another block - this should result in two height changed messages
    let blocks = regtest.generate_blocks(1, address.address).await.unwrap();
    info!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("height changed");
    let request = get_request_from_message(message);
    assert_received_height_changed(request, blocks);
    info!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("height changed");
    let request = get_request_from_message(message);
    assert_received_height_changed(request, blocks);
}

#[tokio::test]
#[timeout(300000)]
#[ignore]
async fn monero_syncer_sweep_test() {
    setup_logging(None);
    let (regtest, wallet) = setup_monero().await;
    let address = wallet.get_address(0, None).await.unwrap();
    let blocks = regtest.generate_blocks(200, address.address).await.unwrap();

    let duration = std::time::Duration::from_secs(20);
    std::thread::sleep(duration);

    let (tx, rx_event) = create_monero_syncer("sweep", false);

    let spend_key = monero::PrivateKey::from_str(
        "77916d0cd56ed1920aef6ca56d8a41bac915b68e4c46a589e0956e27a7b77404",
    )
    .unwrap();
    let view_key = monero::PrivateKey::from_str(
        "8163466f1883598e6dd14027b8da727057165da91485834314f5500a65846f09",
    )
    .unwrap();
    let keypair = monero::KeyPair {
        view: view_key,
        spend: spend_key,
    };
    let to_be_sweeped_address = monero::Address::from_keypair(monero::Network::Mainnet, &keypair);
    let dest_address = monero::Address::from_str("43qHP7gSJJf8HZw1G3ZmpWVyYnbxkKdfta34Qj2nuRENjAsXBtj9JcMWcYMeT3n4NyTZqxhUkKgsTS6P2TNgM6ksM32czSp").unwrap();
    send_monero(&wallet, to_be_sweeped_address, 500000000000).await;

    let task = SyncerdTask {
        task: Task::SweepAddress(SweepAddress {
            id: TaskId(0),
            lifetime: blocks + 40,
            from_height: None,
            addendum: SweepAddressAddendum::Monero(SweepXmrAddress {
                spend_key,
                view_key,
                address: dest_address,
                minimum_balance: monero::Amount::from_pico(1000000000000),
            }),
        }),
        source: SOURCE2.clone(),
    };
    tx.send(task).unwrap();

    // the minimum amount is not reached, so let it mature and later check that no sweep has been executed
    regtest.generate_blocks(20, address.address).await.unwrap();
    let duration = std::time::Duration::from_secs(20);
    std::thread::sleep(duration);

    send_monero(&wallet, to_be_sweeped_address, 500000000000).await;
    regtest.generate_blocks(20, address.address).await.unwrap();

    info!("waiting for sweep address message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received sweep success message");
    let request = get_request_from_message(message);
    assert_sweep_success(request, TaskId(0));

    // check that only a single sweep message has been received
    assert!(rx_event.recv_multipart(1).is_err());
}

/*
We test for the following scenarios in the address transaction tests:

- Submit a WatchAddress task with an address with no history yet, then create a
transaction for it and check the respective event

- Submit a WatchAddress task with another address in parallel, then create two
transactions for it and check for both respective events

- Submit a WatchAddress task with the same address again, observe if it receives
the complete existing transaction history

- Submit a WatchAddress task many times with the same address, ensure we receive
many times the same event

- Submit a WatchAddress task with a from_height to an address with existing
transactions, ensure we receive only events for transactions after the from
height

*/
#[tokio::test]
#[timeout(600000)]
#[ignore]
async fn monero_syncer_address_test() {
    for (socket_name, lws_bool) in [("address", false), ("lws_address", true)] {
        if lws_bool {
            setup_logging(Some(log::LevelFilter::Trace))
        } else {
            setup_logging(None)
        };
        info!(
            "testing {}",
            if lws_bool {
                "monero-lws"
            } else {
                "monero-wallet-rpc"
            }
        );
        let (regtest, wallet) = setup_monero().await;
        let address = wallet.get_address(0, None).await.unwrap();
        regtest.generate_blocks(200, address.address).await.unwrap();

        // allow some time for things to happen, like the wallet server catching up
        let duration = std::time::Duration::from_secs(20);
        std::thread::sleep(duration);

        // create a monero syncer
        let (tx, rx_event) = create_monero_syncer(socket_name, lws_bool);

        // Generate two addresses and watch them
        let (address1, view_key1) = new_address(&wallet).await;
        let tx_id = send_monero(&wallet, address1, 1).await;
        let blocks = regtest.generate_blocks(10, address.address).await.unwrap();

        let duration = std::time::Duration::from_secs(20);
        std::thread::sleep(duration);

        let addendum_1 = AddressAddendum::Monero(XmrAddressAddendum {
            spend_key: address1.public_spend,
            view_key: view_key1,
            from_height: if lws_bool { 0 } else { 10 },
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

        info!(
            "waiting for address transaction message for address {}",
            address1
        );
        let message = rx_event.recv_multipart(0).unwrap();
        info!(
            "received address transaction message for address {}",
            address1
        );
        let request = get_request_from_message(message);
        assert_address_transaction(request, 1, vec![tx_id]);

        // Generate two transactions for same address and watch them
        let (address2, view_key2) = new_address(&wallet).await;
        let tx_id2_1 = send_monero(&wallet, address2, 1).await;
        let tx_id2_2 = send_monero(&wallet, address2, 1).await;
        let blocks = if lws_bool {
            regtest.generate_blocks(1, address.address).await.unwrap()
        } else {
            blocks
        };

        let addendum_2 = AddressAddendum::Monero(XmrAddressAddendum {
            spend_key: address2.public_spend,
            view_key: view_key2,
            from_height: 0,
        });
        let watch_address_task_2 = SyncerdTask {
            task: Task::WatchAddress(WatchAddress {
                id: TaskId(1),
                lifetime: blocks + 1,
                addendum: addendum_2,
                include_tx: Boolean::True,
            }),
            source: SOURCE1.clone(),
        };
        tx.send(watch_address_task_2).unwrap();

        info!("waiting for address transaction message");
        let message = rx_event.recv_multipart(0).unwrap();
        info!("received address transaction message");
        let request = get_request_from_message(message);
        assert_address_transaction(request, 1, vec![tx_id2_1.clone(), tx_id2_2.clone()]);

        info!("waiting for address transaction message");
        let message = rx_event.recv_multipart(0).unwrap();
        info!("received address transaction message");
        let request = get_request_from_message(message);
        assert_address_transaction(request, 1, vec![tx_id2_1.clone(), tx_id2_2.clone()]);

        let addendum_3 = AddressAddendum::Monero(XmrAddressAddendum {
            spend_key: address2.public_spend,
            view_key: view_key2,
            from_height: 0,
        });
        let watch_address_task_3 = SyncerdTask {
            task: Task::WatchAddress(WatchAddress {
                id: TaskId(1),
                lifetime: blocks + 1,
                addendum: addendum_3,
                include_tx: Boolean::True,
            }),
            source: SOURCE1.clone(),
        };
        tx.send(watch_address_task_3).unwrap();
        info!("waiting for address transaction message");
        let message = rx_event.recv_multipart(0).unwrap();
        info!("received address transaction message");
        let request = get_request_from_message(message);
        assert_address_transaction(request, 1, vec![tx_id2_1.clone(), tx_id2_2.clone()]);

        info!("waiting for address transaction message");
        let message = rx_event.recv_multipart(0).unwrap();
        info!("received address transaction message");
        let request = get_request_from_message(message);
        assert_address_transaction(request, 1, vec![tx_id2_1.clone(), tx_id2_2.clone()]);

        let (address4, view_key4) = new_address(&wallet).await;

        let tx_id4 = send_monero(&wallet, address4, 1).await;
        let blocks = if lws_bool {
            regtest.generate_blocks(1, address.address).await.unwrap()
        } else {
            blocks
        };

        let addendum_4 = AddressAddendum::Monero(XmrAddressAddendum {
            spend_key: address4.public_spend,
            view_key: view_key4,
            from_height: 0,
        });
        for i in 0..5 {
            tx.send(SyncerdTask {
                task: Task::WatchAddress(WatchAddress {
                    id: TaskId(i),
                    lifetime: blocks + 5,
                    addendum: addendum_4.clone(),
                    include_tx: Boolean::True,
                }),
                source: SOURCE2.clone(),
            })
            .unwrap();
        }

        for i in 0..5 {
            info!("waiting for repeated address transaction message {}", i);
            let message = rx_event.recv_multipart(0).unwrap();
            info!("received repeated address transaction message {}", i);
            let request = get_request_from_message(message);
            assert_address_transaction(request, 1, vec![tx_id4.clone()]);
        }

        // generate an address, send Monero to it and ensure that the first transaction sent to it does not show up
        let (address5, view_key5) = new_address(&wallet).await;
        send_monero(&wallet, address5, 1).await;
        // this transaction should not generate an event, because the task's lifetime expired
        send_monero(&wallet, address4, 1).await;
        let blocks = regtest.generate_blocks(10, address.address).await.unwrap();

        let addendum_5 = AddressAddendum::Monero(XmrAddressAddendum {
            spend_key: address5.public_spend,
            view_key: view_key5,
            from_height: blocks,
        });

        tx.send(SyncerdTask {
            task: Task::WatchAddress(WatchAddress {
                id: TaskId(5),
                lifetime: blocks + 5,
                addendum: addendum_5.clone(),
                include_tx: Boolean::True,
            }),
            source: SOURCE2.clone(),
        })
        .unwrap();

        let tx_id5_2 = send_monero(&wallet, address5, 2).await;
        if lws_bool {
            regtest.generate_blocks(1, address.address).await.unwrap();
        }
        info!("waiting for address transaction message");
        let message = rx_event.recv_multipart(0).unwrap();
        info!("received address transaction message");
        let request = get_request_from_message(message);
        assert_address_transaction(request, 2, vec![tx_id5_2.clone()]);

        let tx_id5_2_3 = send_monero(&wallet, address5, 2).await;
        regtest.generate_blocks(1, address.address).await.unwrap();
        info!("waiting for address transaction message");
        let message = rx_event.recv_multipart(0).unwrap();
        info!("received address transaction message");
        let request = get_request_from_message(message);
        assert_address_transaction(request, 2, vec![tx_id5_2_3.clone()]);
    }
}

/*
We test for the following scenarios in the transaction tests:

- Submit a WatchTransaction task for a transaction in the mempool, receive confirmation events until
the threshold confs are reached

- Submit a WatchTransaction task for a mined transaction, receive confirmation events

- Submit two WatchTransaction tasks in parallel, receive confirmation events for both

- Create a transaction, but don't relay it, then watch it and receive a tx not
found confirmation event. Then relay and receive further events.
*/
#[tokio::test]
#[timeout(300000)]
#[ignore]
async fn monero_syncer_transaction_test() {
    setup_logging(None);
    let (regtest, wallet) = setup_monero().await;
    let address = wallet.get_address(0, None).await.unwrap().address;
    let blocks = regtest.generate_blocks(200, address).await.unwrap();

    // allow some time for things to happen, like the wallet server catching up
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);

    // create a monero syncer
    let (tx, rx_event) = create_monero_syncer("transaction", false);

    let txid_1 = send_monero(&wallet, address, 1).await;

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
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(0), vec![0]);

    let block_height = regtest.generate_blocks(1, address).await.unwrap();
    let block_hash = get_block_hash_from_height(&regtest, block_height).await;
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(1), block_hash.clone());

    regtest.generate_blocks(1, address).await.unwrap();
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(2), block_hash);

    let tx_id2 = send_monero(&wallet, address, 1).await;
    let block_height = regtest.generate_blocks(1, address).await.unwrap();
    let block_hash = get_block_hash_from_height(&regtest, block_height).await;
    tx.send(SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: TaskId(1),
            lifetime: block_height + 5,
            hash: tx_id2,
            confirmation_bound: 2,
        }),
        source: SOURCE1.clone(),
    })
    .unwrap();
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(1), block_hash.clone());

    regtest.generate_blocks(1, address).await.unwrap();
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(2), block_hash);

    let txid_2 = send_monero(&wallet, address, 1).await;
    let txid_3 = send_monero(&wallet, address, 1).await;

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
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(0), vec![0]);
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(0), vec![0]);

    let options = monero_rpc::TransferOptions {
        account_index: None,
        subaddr_indices: None,
        mixin: None,
        ring_size: None,
        unlock_time: None,
        payment_id: None,
        do_not_relay: Some(true),
    };
    let transaction = wallet
        .transfer(
            [(address, monero::Amount::from_pico(1000))]
                .iter()
                .cloned()
                .collect(),
            monero_rpc::TransferPriority::Default,
            options.clone(),
        )
        .await
        .unwrap();
    tx.send(SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: TaskId(1),
            lifetime: blocks + 5,
            hash: hex::decode(transaction.tx_hash.to_string()).unwrap(),
            confirmation_bound: 2,
        }),
        source: SOURCE1.clone(),
    })
    .unwrap();

    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, None, vec![0]);

    wallet
        .relay_tx(hex::encode(transaction.tx_metadata.0))
        .await
        .unwrap();
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(0), vec![0]);
    let block_height = regtest.generate_blocks(1, address).await.unwrap();
    let block_hash = get_block_hash_from_height(&regtest, block_height).await;
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, Some(1), block_hash);
}

/*
We test for the following scenarios in the abort tests:

- Submit a WatchTransaction task for a non-existing transaction, receive a confirmation events

- Submit an Abort task for the transaction, receive success

- Submit an Abort task for the transaction, receive an error

- Submit two WatchTransaction tasks, abort them both and receive both their aborted id's.
*/
#[tokio::test]
#[timeout(300000)]
#[ignore]
async fn monero_syncer_abort_test() {
    setup_logging(None);
    let (tx, rx_event) = create_monero_syncer("abort", false);
    let (regtest, wallet) = setup_monero().await;
    let address = wallet.get_address(0, None).await.unwrap();
    let blocks = regtest.generate_blocks(1, address.address).await.unwrap();

    let task = SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: TaskId(0),
            lifetime: blocks + 2,
            hash: vec![0; 32],
            confirmation_bound: 2,
        }),
        source: SOURCE2.clone(),
    };
    tx.send(task).unwrap();
    let message = rx_event.recv_multipart(0).unwrap();
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, None, vec![0]);

    let task = SyncerdTask {
        task: Task::Abort(Abort {
            task_target: TaskTarget::TaskId(TaskId(0)),
            respond: Boolean::True,
        }),
        source: SOURCE2.clone(),
    };
    tx.send(task).unwrap();
    info!("waiting for task aborted message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("task aborted");
    let request = get_request_from_message(message);
    assert_task_aborted(request, None, vec![0]);

    let task = SyncerdTask {
        task: Task::Abort(Abort {
            task_target: TaskTarget::TaskId(TaskId(0)),
            respond: Boolean::True,
        }),
        source: SOURCE2.clone(),
    };
    tx.send(task).unwrap();
    info!("waiting for task aborted message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("task aborted");
    let request = get_request_from_message(message);
    assert_task_aborted(
        request,
        Some("abort failed, task from source Monero (Local) syncer not found".to_string()),
        vec![],
    );

    let task = SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: TaskId(0),
            lifetime: blocks + 10,
            hash: vec![0; 32],
            confirmation_bound: 2,
        }),
        source: SOURCE2.clone(),
    };
    tx.send(task).unwrap();
    let message = rx_event.recv_multipart(0).unwrap();
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, None, vec![0]);
    let task = SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: TaskId(1),
            lifetime: blocks + 10,
            hash: vec![0; 32],
            confirmation_bound: 2,
        }),
        source: SOURCE2.clone(),
    };
    tx.send(task).unwrap();
    let message = rx_event.recv_multipart(0).unwrap();
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, None, vec![0]);
    let task = SyncerdTask {
        task: Task::Abort(Abort {
            task_target: TaskTarget::AllTasks,
            respond: Boolean::True,
        }),
        source: SOURCE2.clone(),
    };
    tx.send(task).unwrap();
    info!("waiting for task aborted message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("task aborted");
    let request = get_request_from_message(message);
    assert_task_aborted(request, None, vec![0, 1]);

    let task = SyncerdTask {
        task: Task::Abort(Abort {
            task_target: TaskTarget::TaskId(TaskId(0)),
            respond: Boolean::True,
        }),
        source: SOURCE2.clone(),
    };
    tx.send(task).unwrap();
    info!("waiting for task aborted message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("task aborted");
    let request = get_request_from_message(message);
    assert_task_aborted(
        request,
        Some("abort failed, task from source Monero (Local) syncer not found".to_string()),
        vec![],
    );
}

/*
Check that a monero BroadcastTransaction task generates an error
*/
#[tokio::test]
#[timeout(300000)]
#[ignore]
async fn monero_syncer_broadcast_tx_test() {
    setup_logging(None);
    let (regtest, wallet) = setup_monero().await;
    let address = wallet.get_address(0, None).await.unwrap();
    regtest.generate_blocks(1, address.address).await.unwrap();

    let (tx, rx_event) = create_monero_syncer("broadcast", false);

    let task = SyncerdTask {
        task: Task::BroadcastTransaction(BroadcastTransaction {
            id: TaskId(0),
            tx: vec![0],
        }),
        source: SOURCE2.clone(),
    };
    tx.send(task).unwrap();

    info!("waiting for transaction broadcasted message");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("transaction broadcasted");
    let request = get_request_from_message(message);
    assert_transaction_broadcasted(
        request,
        true,
        Some("broadcast transaction not available for Monero".to_string()),
    );
}

async fn setup_monero() -> (monero_rpc::RegtestDaemonClient, monero_rpc::WalletClient) {
    let conf = config::TestConfig::parse();

    let client = monero_rpc::RpcClient::new(format!("{}", conf.monero.daemon));
    let regtest = client.daemon().regtest();

    let client = monero_rpc::RpcClient::new(format!(
        "{}",
        conf.monero.get_wallet(config::WalletIndex::Primary)
    ));
    let wallet = client.wallet();

    // Ignore if fails, maybe the wallet already exists
    let _ = wallet
        .create_wallet("test".to_string(), None, "English".to_string())
        .await;
    wallet
        .open_wallet("test".to_string(), None)
        .await
        .expect("The wallet exists, created the line before");
    (regtest, wallet)
}

fn create_monero_syncer(
    socket_name: &str,
    lws: bool,
) -> (std::sync::mpsc::Sender<SyncerdTask>, zmq::Socket) {
    let addr = format!("inproc://testmonerobridge-{}", socket_name);
    let (tx, rx): (Sender<SyncerdTask>, Receiver<SyncerdTask>) = std::sync::mpsc::channel();
    let tx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
    let rx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
    tx_event.connect(&addr).unwrap();
    rx_event.bind(&addr).unwrap();
    let mut syncer = MoneroSyncer::new();

    let conf = config::TestConfig::parse();
    let lws_wallet = &format!("{}", conf.monero.lws);
    let wallet_server = if lws {
        vec!["--monero-lws", lws_wallet]
    } else {
        vec![]
    };

    let opts = Opts::parse_from(
        vec!["syncerd"]
            .into_iter()
            .chain(vec![
                "--coin",
                "Monero",
                "--monero-daemon",
                &format!("{}", conf.monero.daemon),
                "--monero-rpc-wallet",
                &format!("{}", conf.monero.get_wallet(config::WalletIndex::Secondary)),
            ])
            .chain(wallet_server),
    );

    syncer
        .run(
            rx,
            tx_event,
            SOURCE2.clone().into(),
            &opts,
            Network::Local,
            true,
        )
        .expect("Invalid monero syncer");
    (tx, rx_event)
}

async fn new_address(wallet: &monero_rpc::WalletClient) -> (monero::Address, monero::PrivateKey) {
    // let address = wallet.create_address(0, None).await.unwrap().0;
    let wallet_name: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect();
    let _ = wallet
        .create_wallet(wallet_name.clone(), None, "English".to_string())
        .await;
    wallet.open_wallet(wallet_name, None).await.unwrap();
    let address = wallet.get_address(0, None).await.unwrap().address;
    let viewkey = wallet
        .query_key(monero_rpc::PrivateKeyType::View)
        .await
        .unwrap();
    wallet.open_wallet("test".to_string(), None).await.unwrap();
    (address, viewkey)
}

async fn send_monero(
    wallet: &monero_rpc::WalletClient,
    address: monero::Address,
    amount: u64,
) -> Vec<u8> {
    let options = monero_rpc::TransferOptions {
        account_index: None,
        subaddr_indices: None,
        mixin: None,
        ring_size: None,
        unlock_time: None,
        payment_id: None,
        do_not_relay: None,
    };
    let transaction = wallet
        .transfer(
            [(address, monero::Amount::from_pico(amount))]
                .iter()
                .cloned()
                .collect(),
            monero_rpc::TransferPriority::Default,
            options.clone(),
        )
        .await
        .unwrap();
    hex::decode(transaction.tx_hash.to_string()).unwrap()
}

async fn get_block_hash_from_height(
    regtest: &monero_rpc::RegtestDaemonClient,
    height: u64,
) -> Vec<u8> {
    let header = regtest
        .get_block_header(GetBlockHeaderSelector::Height(height))
        .await
        .unwrap();
    header.hash.0.to_vec()
}

fn get_request_from_message(message: Vec<Vec<u8>>) -> Request {
    // Receive a Request
    let unmarshaller = Request::create_unmarshaller();
    let mut transcoder = PlainTranscoder {};
    let routed_message = recv_routed(message);
    let plain_message = transcoder.decrypt(routed_message.msg).unwrap();
    (&*unmarshaller.unmarshall(&*plain_message).unwrap()).clone()
}

// as taken from the rust-internet2 crate - for now we only use the message
// field, but there is value in parsing all for visibiliy and testing routing
// information
fn recv_routed(message: std::vec::Vec<std::vec::Vec<u8>>) -> RoutedFrame {
    let mut multipart = message.into_iter();
    // Skipping previous hop data since we do not need them
    let hop = multipart.next().unwrap();
    let src = multipart.next().unwrap();
    let dst = multipart.next().unwrap();
    let msg = multipart.next().unwrap();
    if multipart.count() > 0 {
        panic!("multipart message empty");
    }
    let len = msg.len();
    if len > MAX_FRAME_SIZE as usize {
        panic!(
            "multipart message frame
size too big"
        );
    }
    RoutedFrame { hop, src, dst, msg }
}

fn assert_address_transaction(
    request: Request,
    expected_amount: u64,
    possible_txids: Vec<Vec<u8>>,
) {
    match request {
        Request::SyncerdBridgeEvent(event) => match event.event {
            Event::AddressTransaction(address_transaction) => {
                assert_eq!(address_transaction.amount, expected_amount);
                assert!(possible_txids.contains(&address_transaction.hash));
            }
            _ => panic!("expected address transaction event"),
        },
        _ => panic!("expected syncerd bridge event"),
    }
}

fn assert_sweep_success(request: Request, id: TaskId) {
    match request {
        Request::SyncerdBridgeEvent(event) => match event.event {
            Event::SweepSuccess(sweep_success) => {
                assert_eq!(sweep_success.id, id);
            }
            _ => panic!("expected address transaction event"),
        },
        _ => panic!("expected syncerd bridge event"),
    }
}

fn assert_received_height_changed(request: Request, expected_height: u64) {
    match request {
        Request::SyncerdBridgeEvent(event) => match event.event {
            Event::HeightChanged(height_changed) => {
                assert_eq!(height_changed.height, expected_height);
            }
            _ => {
                panic!("expected height changed event");
            }
        },
        _ => {
            panic!("expected syncerd bridge event");
        }
    }
}

fn assert_transaction_confirmations(
    request: Request,
    expected_confirmations: Option<u32>,
    expected_block_hash: Vec<u8>,
) {
    match request {
        Request::SyncerdBridgeEvent(event) => match event.event {
            Event::TransactionConfirmations(transaction_confirmations) => {
                assert_eq!(
                    transaction_confirmations.confirmations,
                    expected_confirmations
                );
                assert_eq!(transaction_confirmations.block, expected_block_hash);
            }
            _ => panic!("expected address transaction event"),
        },
        _ => panic!("expected syncerd bridge event"),
    }
}

fn assert_task_aborted(
    request: Request,
    expected_error: Option<String>,
    mut expected_id: Vec<u32>,
) {
    match request {
        Request::SyncerdBridgeEvent(event) => match event.event {
            Event::TaskAborted(mut task_aborted) => {
                assert_eq!(
                    &task_aborted.id.sort_unstable(),
                    &expected_id.sort_unstable()
                );
                assert_eq!(task_aborted.error, expected_error);
            }
            _ => {
                panic!("expected task aborted event");
            }
        },
        _ => {
            panic!("expected syncerd bridge event");
        }
    }
}

fn assert_transaction_broadcasted(request: Request, has_error: bool, error_msg: Option<String>) {
    match request {
        Request::SyncerdBridgeEvent(event) => match event.event {
            Event::TransactionBroadcasted(transaction_broadcasted) => {
                if has_error {
                    assert!(transaction_broadcasted.error.is_some());
                    if error_msg.is_some() {
                        assert_eq!(transaction_broadcasted.error.unwrap(), error_msg.unwrap());
                    }
                } else {
                    assert!(transaction_broadcasted.error.is_none());
                }
            }
            _ => {
                panic!("expected height changed event");
            }
        },
        _ => {
            panic!("expected syncerd bridge event");
        }
    }
}

fn setup_logging(level: Option<log::LevelFilter>) {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(level.unwrap_or(log::LevelFilter::Info))
        .try_init();
}
