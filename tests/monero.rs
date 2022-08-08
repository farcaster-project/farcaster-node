use clap::Parser;
use farcaster_core::blockchain::{Blockchain, Network};
use farcaster_node::syncerd::monero_syncer::MoneroSyncer;
use farcaster_node::syncerd::opts::Opts;
use farcaster_node::syncerd::runtime::SyncerdTask;
use farcaster_node::syncerd::types::{
    Abort, AddressAddendum, Boolean, BroadcastTransaction, Task, WatchAddress, WatchHeight,
    WatchTransaction,
};
use farcaster_node::syncerd::{
    runtime::Synclet, SweepAddress, SweepAddressAddendum, SweepMoneroAddress, TaskId, TaskTarget,
    XmrAddressAddendum,
};
use farcaster_node::ServiceId;
use microservices::ZMQ_CONTEXT;
use monero_rpc::GetBlockHeaderSelector;
use ntest::timeout;
use rand::{distributions::Alphanumeric, Rng};
use std::str::FromStr;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;

use utils::assert;
use utils::config;
use utils::misc;
use utils::setup_logging;

#[macro_use]
extern crate log;

mod utils;

const SOURCE1: ServiceId = ServiceId::Syncer(Blockchain::Bitcoin, Network::Local);
const SOURCE2: ServiceId = ServiceId::Syncer(Blockchain::Monero, Network::Local);

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
    setup_logging();
    let (regtest, wallet) = setup_monero().await;
    let address = wallet.get_address(0, None).await.unwrap();
    let blocks = regtest
        .generate_blocks(1, address.address)
        .await
        .unwrap()
        .height;

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
    let request = misc::get_request_from_message(message);
    assert::received_height_changed(request, blocks);
    // Generate a single height changed event
    let blocks = regtest
        .generate_blocks(1, address.address)
        .await
        .unwrap()
        .height;
    info!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("height changed");
    let request = misc::get_request_from_message(message);
    // let blocks = regtest.get_block_count().await.unwrap();
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
    info!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("height changed");
    let request = misc::get_request_from_message(message);
    assert::received_height_changed(request, blocks);

    // generate another block - this should result in two height changed messages
    let blocks = regtest
        .generate_blocks(1, address.address)
        .await
        .unwrap()
        .height;
    info!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("height changed");
    let request = misc::get_request_from_message(message);
    assert::received_height_changed(request, blocks);
    info!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("height changed");
    let request = misc::get_request_from_message(message);
    assert::received_height_changed(request, blocks);
}

#[tokio::test]
#[timeout(300000)]
#[ignore]
async fn monero_syncer_sweep_test() {
    setup_logging();
    let (regtest, wallet) = setup_monero().await;
    let address = wallet.get_address(0, None).await.unwrap();
    let blocks = regtest
        .generate_blocks(200, address.address)
        .await
        .unwrap()
        .height;

    let duration = std::time::Duration::from_secs(20);
    std::thread::sleep(duration);

    let (tx, rx_event) = create_monero_syncer("sweep", false);

    let source_spend_key = monero::PrivateKey::from_str(
        "77916d0cd56ed1920aef6ca56d8a41bac915b68e4c46a589e0956e27a7b77404",
    )
    .unwrap();
    let source_view_key = monero::PrivateKey::from_str(
        "8163466f1883598e6dd14027b8da727057165da91485834314f5500a65846f09",
    )
    .unwrap();
    let keypair = monero::KeyPair {
        view: source_view_key,
        spend: source_spend_key,
    };
    let to_be_sweeped_address = monero::Address::from_keypair(monero::Network::Mainnet, &keypair);
    let destination_address = monero::Address::from_str("43qHP7gSJJf8HZw1G3ZmpWVyYnbxkKdfta34Qj2nuRENjAsXBtj9JcMWcYMeT3n4NyTZqxhUkKgsTS6P2TNgM6ksM32czSp").unwrap();
    send_monero(&wallet, to_be_sweeped_address, 500000000000).await;

    let task = SyncerdTask {
        task: Task::SweepAddress(SweepAddress {
            id: TaskId(0),
            lifetime: blocks + 40,
            from_height: None,
            retry: true,
            addendum: SweepAddressAddendum::Monero(SweepMoneroAddress {
                source_spend_key,
                source_view_key,
                destination_address,
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
    let request = misc::get_request_from_message(message);
    assert::sweep_success(request, TaskId(0));

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
    // TODO enable `lws_address` when the lws wallet starts working with v0.18.0.0
    for (socket_name, lws_bool) in [("address", false) /*("lws_address", true)*/] {
        if lws_bool {
            std::env::set_var("RUST_LOG", "farcaster_node=trace,monero-lws=trace");
            setup_logging()
        } else {
            setup_logging()
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
        let blocks = regtest
            .generate_blocks(10, address.address)
            .await
            .unwrap()
            .height;

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
        let request = misc::get_request_from_message(message);
        assert::address_transaction(request, 1, vec![tx_id]);

        // Generate two transactions for same address and watch them
        let (address2, view_key2) = new_address(&wallet).await;
        let tx_id2_1 = send_monero(&wallet, address2, 1).await;
        let tx_id2_2 = send_monero(&wallet, address2, 1).await;
        let blocks = if lws_bool {
            regtest
                .generate_blocks(1, address.address)
                .await
                .unwrap()
                .height
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
        let request = misc::get_request_from_message(message);
        assert::address_transaction(request, 1, vec![tx_id2_1.clone(), tx_id2_2.clone()]);

        info!("waiting for address transaction message");
        let message = rx_event.recv_multipart(0).unwrap();
        info!("received address transaction message");
        let request = misc::get_request_from_message(message);
        assert::address_transaction(request, 1, vec![tx_id2_1.clone(), tx_id2_2.clone()]);

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
        let request = misc::get_request_from_message(message);
        assert::address_transaction(request, 1, vec![tx_id2_1.clone(), tx_id2_2.clone()]);

        info!("waiting for address transaction message");
        let message = rx_event.recv_multipart(0).unwrap();
        info!("received address transaction message");
        let request = misc::get_request_from_message(message);
        assert::address_transaction(request, 1, vec![tx_id2_1.clone(), tx_id2_2.clone()]);

        let (address4, view_key4) = new_address(&wallet).await;

        let tx_id4 = send_monero(&wallet, address4, 1).await;
        let blocks = if lws_bool {
            regtest
                .generate_blocks(1, address.address)
                .await
                .unwrap()
                .height
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
            let request = misc::get_request_from_message(message);
            assert::address_transaction(request, 1, vec![tx_id4.clone()]);
        }

        // generate an address, send Monero to it and ensure that the first transaction sent to it does not show up
        let (address5, view_key5) = new_address(&wallet).await;
        send_monero(&wallet, address5, 1).await;
        // this transaction should not generate an event, because the task's lifetime expired
        send_monero(&wallet, address4, 1).await;
        let blocks = regtest
            .generate_blocks(10, address.address)
            .await
            .unwrap()
            .height;

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
        let request = misc::get_request_from_message(message);
        assert::address_transaction(request, 2, vec![tx_id5_2.clone()]);

        let tx_id5_2_3 = send_monero(&wallet, address5, 2).await;
        regtest.generate_blocks(1, address.address).await.unwrap();
        info!("waiting for address transaction message");
        let message = rx_event.recv_multipart(0).unwrap();
        info!("received address transaction message");
        let request = misc::get_request_from_message(message);
        assert::address_transaction(request, 2, vec![tx_id5_2_3.clone()]);
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
    setup_logging();
    let (regtest, wallet) = setup_monero().await;
    let address = wallet.get_address(0, None).await.unwrap().address;
    let blocks = regtest.generate_blocks(200, address).await.unwrap().height;

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
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(0), vec![0]);

    let block_height = regtest.generate_blocks(1, address).await.unwrap().height;
    let block_hash = get_block_hash_from_height(&regtest, block_height).await;
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(1), block_hash.clone());

    regtest.generate_blocks(1, address).await.unwrap();
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(2), block_hash);

    let tx_id2 = send_monero(&wallet, address, 1).await;
    let block_height = regtest.generate_blocks(1, address).await.unwrap().height;
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
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(1), block_hash.clone());

    regtest.generate_blocks(1, address).await.unwrap();
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(2), block_hash);

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
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(0), vec![0]);
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(0), vec![0]);

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
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, None, vec![0]);

    wallet
        .relay_tx(hex::encode(transaction.tx_metadata.0))
        .await
        .unwrap();
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(0), vec![0]);
    let block_height = regtest.generate_blocks(1, address).await.unwrap().height;
    let block_hash = get_block_hash_from_height(&regtest, block_height).await;
    info!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    info!("received confirmation");
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, Some(1), block_hash);
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
    setup_logging();
    let (tx, rx_event) = create_monero_syncer("abort", false);
    let (regtest, wallet) = setup_monero().await;
    let address = wallet.get_address(0, None).await.unwrap();
    let blocks = regtest
        .generate_blocks(1, address.address)
        .await
        .unwrap()
        .height;

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
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, None, vec![0]);

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
    let request = misc::get_request_from_message(message);
    assert::task_aborted(request, None, vec![0]);

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
    let request = misc::get_request_from_message(message);
    assert::task_aborted(
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
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, None, vec![0]);
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
    let request = misc::get_request_from_message(message);
    assert::transaction_confirmations(request, None, vec![0]);
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
    let request = misc::get_request_from_message(message);
    assert::task_aborted(request, None, vec![0, 1]);

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
    let request = misc::get_request_from_message(message);
    assert::task_aborted(
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
    setup_logging();
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
    let request = misc::get_request_from_message(message);
    assert::transaction_broadcasted(
        request,
        true,
        Some("broadcast transaction not available for Monero".to_string()),
    );
}

// =========================
// TODO: move into utils from here
//

async fn setup_monero() -> (
    monero_rpc::RegtestDaemonJsonRpcClient,
    monero_rpc::WalletClient,
) {
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
    use rand::prelude::*;
    let mut rng = rand::thread_rng();
    let id: u64 = rng.gen();
    let addr = format!("inproc://testmonerobridge-{}-{}", socket_name, id);

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
                "--blockchain",
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
        .expect("Invalid Monero syncer!");
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
    regtest: &monero_rpc::RegtestDaemonJsonRpcClient,
    height: u64,
) -> Vec<u8> {
    let header = regtest
        .get_block_header(GetBlockHeaderSelector::Height(height))
        .await
        .unwrap();
    header.hash.0.to_vec()
}
