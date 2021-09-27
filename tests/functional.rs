use bitcoin::BlockHash;
use bitcoincore_rpc::{Auth, Client, RpcApi};
use farcaster_node::rpc::Request;
use farcaster_node::syncerd::bitcoin_syncer::BitcoinSyncer;
use farcaster_node::syncerd::bitcoin_syncer::Synclet;
use farcaster_node::syncerd::monero_syncer::MoneroSyncer;
use farcaster_node::syncerd::opts::Coin;
use farcaster_node::syncerd::runtime::SyncerdTask;
use farcaster_node::syncerd::SyncerServers;
use farcaster_node::syncerd::XmrAddressAddendum;
use farcaster_node::ServiceId;
use internet2::transport::MAX_FRAME_SIZE;
use internet2::Decrypt;
use internet2::PlainTranscoder;
use internet2::RoutedFrame;
use internet2::ZMQ_CONTEXT;
use lnpbp::chain::Chain;
use monero::Address;
use monero_rpc::GetBlockHeaderSelector;
use std::collections::HashMap;
use std::convert::TryInto;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;

use bitcoin::hashes::Hash;
use internet2::{CreateUnmarshaller, Unmarshall};
use std::str::FromStr;

use farcaster_node::syncerd::types::{
    AddressAddendum, Boolean, BroadcastTransaction, BtcAddressAddendum, Event, Task, WatchAddress,
    WatchHeight, WatchTransaction,
};

/*
These tests need to run serialy, otherwise we cannot verify events based on the
state of electrum and bitcoin
*/

#[test]
#[ignore] // it's too expensive
fn bitcoin_syncer_test() {
    bitcoin_syncer_block_height_test(true);
    bitcoin_syncer_address_test(true);
    bitcoin_syncer_transaction_test(true);
    bitcoin_syncer_broadcast_tx_test(true);
    bitcoin_syncer_block_height_test(false);
    bitcoin_syncer_address_test(false);
    bitcoin_syncer_transaction_test(false);
    bitcoin_syncer_broadcast_tx_test(false);
}

#[tokio::test]
#[ignore] // it's too expensive
async fn monero_syncer_test() {
    monero_syncer_block_height_test().await;
    monero_syncer_address_test().await;
    monero_syncer_transaction_test().await;
    monero_syncer_broadcast_tx_test().await;
}

/*
We test for the following scenarios in the block height tests:

- Submit a WatchHeight task, and immediately receive a HeightChanged event

- Mine a block and receive a single HeightChanged event

- Submit another WatchHeigh task,and immediately receive a HeightChanged event

- Mine another block and receive two HeightChanged events
*/
fn bitcoin_syncer_block_height_test(polling: bool) {
    let bitcoin_rpc = bitcoin_setup();
    let address = bitcoin_rpc.get_new_address(None, None).unwrap();

    // start a bitcoin syncer
    let (tx, rx_event) = create_bitcoin_syncer(polling);

    let blocks = bitcoin_rpc.get_block_count().unwrap();

    // Send a WatchHeight task
    let task = SyncerdTask {
        task: Task::WatchHeight(WatchHeight {
            id: 0,
            lifetime: blocks + 2,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    };
    tx.send(task).unwrap();

    // Receive the request and compare it to the actual block count
    println!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("height changed");
    let request = get_request_from_message(message);
    let blocks = bitcoin_rpc.get_block_count().unwrap();
    assert_received_height_changed(request, blocks);
    // Generate a single height changed event
    bitcoin_rpc.generate_to_address(1, &address).unwrap();
    println!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("height changed");
    let request = get_request_from_message(message);
    let blocks = bitcoin_rpc.get_block_count().unwrap();
    assert_received_height_changed(request, blocks);

    // Send another WatchHeight task
    let task = SyncerdTask {
        task: Task::WatchHeight(WatchHeight {
            id: 1,
            lifetime: blocks + 2,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    };
    tx.send(task).unwrap();
    let message = rx_event.recv_multipart(0).unwrap();
    let request = get_request_from_message(message);
    assert_received_height_changed(request, blocks);

    // generate another block - this should result in two height changed messages
    bitcoin_rpc.generate_to_address(1, &address).unwrap();
    println!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("height changed");
    let request = get_request_from_message(message);
    let blocks = bitcoin_rpc.get_block_count().unwrap();
    assert_received_height_changed(request, blocks);
    println!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("height changed");
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
*/
fn bitcoin_syncer_address_test(polling: bool) {
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
    let (tx, rx_event) = create_bitcoin_syncer(polling);

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
            id: 1,
            lifetime: blocks + 1,
            addendum: addendum_1,
            include_tx: Boolean::True,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    };
    tx.send(watch_address_task_1).unwrap();
    let watch_address_task_2 = SyncerdTask {
        task: Task::WatchAddress(WatchAddress {
            id: 1,
            lifetime: blocks + 2,
            addendum: addendum_2.clone(),
            include_tx: Boolean::True,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    };
    tx.send(watch_address_task_2).unwrap();

    // send some coins to address1
    let txid = bitcoin_rpc
        .send_to_address(&address1, amount, None, None, None, None, None, None)
        .unwrap();
    println!("waiting for watch transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received transaction message");
    let request = get_request_from_message(message);
    assert_address_transaction(request, amount.as_sat(), vec![txid.to_vec()]);

    // now generate a block for address1, then wait for the response and test it
    let block_hash = bitcoin_rpc.generate_to_address(1, &address1).unwrap();
    println!("waiting for watch transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received transaction message");
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
    println!("waiting for watch transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received transaction message");
    let request = get_request_from_message(message);
    assert_address_transaction(
        request,
        amount.as_sat(),
        vec![txid_1.to_vec(), txid_2.to_vec()],
    );

    println!("waiting for watch transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received transaction message");
    let request = get_request_from_message(message);
    assert_address_transaction(
        request,
        amount.as_sat(),
        vec![txid_1.to_vec(), txid_2.to_vec()],
    );

    // watch for the same address, it should already contain transactions
    let watch_address_task_3 = SyncerdTask {
        task: Task::WatchAddress(WatchAddress {
            id: 1,
            lifetime: blocks + 2,
            addendum: addendum_2,
            include_tx: Boolean::True,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    };
    tx.send(watch_address_task_3).unwrap();
    println!("waiting for watch transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received transaction message");
    let request = get_request_from_message(message);
    assert_address_transaction(
        request,
        amount.as_sat(),
        vec![txid_1.to_vec(), txid_2.to_vec()],
    );
    println!("waiting for watch transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received transaction message");
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
                id: i,
                lifetime: blocks + 5,
                addendum: addendum_4.clone(),
                include_tx: Boolean::True,
            }),
            source: ServiceId::Syncer(Coin::Bitcoin),
        })
        .unwrap();
    }
    let txid = bitcoin_rpc
        .send_to_address(&address4, amount, None, None, None, None, None, None)
        .unwrap();

    for _ in 0..5 {
        println!("waiting for repeated watch transaction message");
        let message = rx_event.recv_multipart(0).unwrap();
        println!("received repeated transaction message");
        let request = get_request_from_message(message);
        assert_address_transaction(request, amount.as_sat(), vec![txid.to_vec()]);
    }
}

/*
We test for the following scenarios in the transaction tests:

- Submit a WatchTransaction task for a transaction in the mempool, receive confirmation events until
the threshold confs are reached

- Submit a WatchTransaction task for a mined transaction, receive confirmation events

- Submit two WatchTransaction tasks in parallel, receive confirmation events for both
*/
fn bitcoin_syncer_transaction_test(polling: bool) {
    let bitcoin_rpc = bitcoin_setup();

    // generate some blocks to an address
    let address = bitcoin_rpc.get_new_address(None, None).unwrap();
    bitcoin_rpc.generate_to_address(110, &address).unwrap();

    // start a bitcoin syncer
    let (tx, rx_event) = create_bitcoin_syncer(polling);

    // 294 Satoshi is the dust limit for a segwit transaction
    let amount = bitcoin::Amount::ONE_SAT * 294;

    // allow some time for things to happen, like the electrum server catching up
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);

    let blocks = bitcoin_rpc.get_block_count().unwrap();
    let txid_1 = bitcoin_rpc
        .send_to_address(&address, amount, None, None, None, None, None, None)
        .unwrap();
    std::thread::sleep(duration);

    tx.send(SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: 1,
            lifetime: blocks + 5,
            hash: txid_1.to_vec(),
            confirmation_bound: 2,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    })
    .unwrap();

    println!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, 0, vec![0]);

    let block_hash = bitcoin_rpc.generate_to_address(1, &address).unwrap();
    println!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, 1, block_hash[0].to_vec());

    bitcoin_rpc.generate_to_address(1, &address).unwrap();
    println!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, 2, block_hash[0].to_vec());

    let block_hash = bitcoin_rpc.generate_to_address(1, &address).unwrap();
    let block = bitcoin_rpc.get_block(&block_hash[0]).unwrap();
    let address_txid = find_coinbase_transaction_id(block.txdata);
    tx.send(SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: 1,
            lifetime: blocks + 5,
            hash: address_txid.to_vec(),
            confirmation_bound: 2,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    })
    .unwrap();
    println!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, 1, block_hash[0].to_vec());

    bitcoin_rpc.generate_to_address(1, &address).unwrap();
    println!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, 2, block_hash[0].to_vec());

    let txid_2 = bitcoin_rpc
        .send_to_address(&address, amount, None, None, None, None, None, None)
        .unwrap();
    let txid_3 = bitcoin_rpc
        .send_to_address(&address, amount, None, None, None, None, None, None)
        .unwrap();

    tx.send(SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: 1,
            lifetime: blocks + 5,
            hash: txid_2.to_vec(),
            confirmation_bound: 2,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    })
    .unwrap();
    tx.send(SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: 1,
            lifetime: blocks + 5,
            hash: txid_3.to_vec(),
            confirmation_bound: 2,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    })
    .unwrap();

    println!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, 0, vec![0]);
    println!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, 0, vec![0]);
}

/*
We test the following scenarios in the broadcast tx tests:

- Submit a BroadcastTransaction task, receive an event with a double spend error

- Submit a BroadcastTransaction task, receive a success event
*/
fn bitcoin_syncer_broadcast_tx_test(polling: bool) {
    let bitcoin_rpc = bitcoin_setup();
    let address = bitcoin_rpc.get_new_address(None, None).unwrap();

    let (tx, rx_event) = create_bitcoin_syncer(polling);

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
            id: 0,
            tx: transaction.hex,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    };
    tx.send(task).unwrap();

    println!("waiting for transaction broadcasted message");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("transaction broadcasted");
    let request = get_request_from_message(message);
    assert_transaction_broadcasted(request, true, None);

    use amplify::map;

    let utxos = bitcoin_rpc
        .list_unspent(Some(100), None, None, Some(false), None)
        .unwrap();
    let bitcoincore_rpc::bitcoincore_rpc_json::ListUnspentResultEntry {
        txid,
        vout,
        amount: in_amount,
        ..
    } = &utxos[0];

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
            id: 0,
            tx: signed_tx.hex,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    };
    tx.send(task).unwrap();

    println!("waiting for transaction broadcasted message");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("transaction broadcasted");
    let request = get_request_from_message(message);
    assert_transaction_broadcasted(request, false, None);
}

fn create_bitcoin_syncer(polling: bool) -> (std::sync::mpsc::Sender<SyncerdTask>, zmq::Socket) {
    let (tx, rx): (Sender<SyncerdTask>, Receiver<SyncerdTask>) = std::sync::mpsc::channel();
    let tx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
    let rx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
    tx_event.connect("inproc://testbitcoinbridge").unwrap();
    rx_event.bind("inproc://testbitcoinbridge").unwrap();
    let mut syncer = BitcoinSyncer::new();
    let syncer_servers = SyncerServers {
        electrum_server: "tcp://localhost:50001".to_string(),
        monero_daemon: "".to_string(),
        monero_rpc_wallet: "".to_string(),
    };
    let dummy = BlockHash::from_slice(&[0; 32]).unwrap();
    syncer.run(
        rx,
        tx_event,
        ServiceId::Syncer(Coin::Bitcoin).into(),
        syncer_servers,
        Chain::Regtest(dummy),
        polling,
    );
    (tx, rx_event)
}

fn find_coinbase_transaction_id(txs: Vec<bitcoin::Transaction>) -> bitcoin::Txid {
    for transaction in txs {
        if transaction.input[0].previous_output.txid
            == bitcoin::Txid::from_slice(&vec![0; 32]).unwrap()
        {
            return transaction.txid();
        }
    }
    bitcoin::Txid::from_slice(&vec![0; 32]).unwrap()
}

fn find_coinbase_transaction_amount(txs: Vec<bitcoin::Transaction>) -> u64 {
    for transaction in txs {
        if transaction.input[0].previous_output.txid
            == bitcoin::Txid::from_slice(&vec![0; 32]).unwrap()
        {
            return transaction.output[0].value;
        }
    }
    0
}

fn bitcoin_setup() -> bitcoincore_rpc::Client {
    let path = std::path::PathBuf::from_str("tests/data_dir/regtest/.cookie").unwrap();
    let bitcoin_rpc = Client::new("http://localhost:18443", Auth::CookieFile(path)).unwrap();

    // make sure a wallet is created and loaded
    match bitcoin_rpc.create_wallet("wallet", None, None, None, None) {
        Err(_e) => match bitcoin_rpc.load_wallet("wallet") {
            _ => {}
        },
        _ => {}
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
async fn monero_syncer_block_height_test() {
    let (regtest, wallet) = setup_monero().await;
    let address = wallet.get_address(0, None).await.unwrap();
    let blocks = regtest.generate_blocks(1, address.address).await.unwrap();

    // allow some time for things to happen, like the wallet server catching up
    let duration = std::time::Duration::from_secs(1);
    std::thread::sleep(duration);

    // create a monero syncer
    let (tx, rx_event) = create_monero_syncer();

    // Send a WatchHeight task
    let task = SyncerdTask {
        task: Task::WatchHeight(WatchHeight {
            id: 0,
            lifetime: blocks + 2,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    };
    tx.send(task).unwrap();

    // Receive the request and compare it to the actual block count
    println!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("height changed");
    let request = get_request_from_message(message);
    assert_received_height_changed(request, blocks);
    // Generate a single height changed event
    let blocks = regtest.generate_blocks(1, address.address).await.unwrap();
    println!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("height changed");
    let request = get_request_from_message(message);
    // let blocks = regtest.get_block_count().await.unwrap();
    assert_received_height_changed(request, blocks);

    // Send another WatchHeight task
    let task = SyncerdTask {
        task: Task::WatchHeight(WatchHeight {
            id: 1,
            lifetime: u64::from(blocks) + 2,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    };
    tx.send(task).unwrap();
    println!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("height changed");
    let request = get_request_from_message(message);
    assert_received_height_changed(request, blocks);

    // generate another block - this should result in two height changed messages
    let blocks = regtest.generate_blocks(1, address.address).await.unwrap();
    println!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("height changed");
    let request = get_request_from_message(message);
    assert_received_height_changed(request, blocks);
    println!("waiting for height changed");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("height changed");
    let request = get_request_from_message(message);
    assert_received_height_changed(request, blocks);
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
*/
async fn monero_syncer_address_test() {
    let (regtest, wallet) = setup_monero().await;
    let address = wallet.get_address(0, None).await.unwrap();
    let blocks = regtest.generate_blocks(200, address.address).await.unwrap();

    // allow some time for things to happen, like the wallet server catching up
    let duration = std::time::Duration::from_secs(20);
    std::thread::sleep(duration);

    // create a monero syncer
    let (tx, rx_event) = create_monero_syncer();

    // Generate two addresses and watch them
    let view_key = wallet.query_view_key().await.unwrap();
    let address1 = wallet.create_address(0, None).await.unwrap().0;
    let tx_id = send_monero(&wallet, address1, 1).await;

    let addendum_1 = AddressAddendum::Monero(XmrAddressAddendum {
        spend_key: address1.public_spend.as_bytes().try_into().unwrap(),
        view_key: view_key.as_bytes().try_into().unwrap(),
        from_height: 0,
    });
    let watch_address_task_1 = SyncerdTask {
        task: Task::WatchAddress(WatchAddress {
            id: 1,
            lifetime: blocks + 1,
            addendum: addendum_1,
            include_tx: Boolean::True,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    };
    tx.send(watch_address_task_1).unwrap();

    println!("waiting for watch transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received transaction message");
    let request = get_request_from_message(message);
    assert_address_transaction(request, 1, vec![tx_id]);

    // Generate two addresses and watch them
    let address2 = wallet.create_address(0, None).await.unwrap().0;
    let tx_id2_1 = send_monero(&wallet, address2, 1).await;
    let tx_id2_2 = send_monero(&wallet, address2, 1).await;

    let addendum_2 = AddressAddendum::Monero(XmrAddressAddendum {
        spend_key: address2.public_spend.as_bytes().try_into().unwrap(),
        view_key: view_key.as_bytes().try_into().unwrap(),
        from_height: 0,
    });
    let watch_address_task_2 = SyncerdTask {
        task: Task::WatchAddress(WatchAddress {
            id: 1,
            lifetime: blocks + 1,
            addendum: addendum_2,
            include_tx: Boolean::True,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    };
    tx.send(watch_address_task_2).unwrap();

    println!("waiting for address transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received address transaction message");
    let request = get_request_from_message(message);
    assert_address_transaction(request, 1, vec![tx_id2_1.clone(), tx_id2_2.clone()]);

    println!("waiting for address transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received address transaction message");
    let request = get_request_from_message(message);
    assert_address_transaction(request, 1, vec![tx_id2_1.clone(), tx_id2_2.clone()]);

    let addendum_3 = AddressAddendum::Monero(XmrAddressAddendum {
        spend_key: address2.public_spend.as_bytes().try_into().unwrap(),
        view_key: view_key.as_bytes().try_into().unwrap(),
        from_height: 0,
    });
    let watch_address_task_3 = SyncerdTask {
        task: Task::WatchAddress(WatchAddress {
            id: 1,
            lifetime: blocks + 1,
            addendum: addendum_3,
            include_tx: Boolean::True,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    };
    tx.send(watch_address_task_3).unwrap();
    println!("waiting for address transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received address transaction message");
    let request = get_request_from_message(message);
    assert_address_transaction(request, 1, vec![tx_id2_1.clone(), tx_id2_2.clone()]);

    println!("waiting for address transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received address transaction message");
    let request = get_request_from_message(message);
    assert_address_transaction(request, 1, vec![tx_id2_1.clone(), tx_id2_2.clone()]);

    let address4 = wallet.create_address(0, None).await.unwrap().0;
    let tx_id4 = send_monero(&wallet, address4, 1).await;

    let addendum_4 = AddressAddendum::Monero(XmrAddressAddendum {
        spend_key: address4.public_spend.as_bytes().try_into().unwrap(),
        view_key: view_key.as_bytes().try_into().unwrap(),
        from_height: 0,
    });
    for i in 0..5 {
        tx.send(SyncerdTask {
            task: Task::WatchAddress(WatchAddress {
                id: i,
                lifetime: blocks + 5,
                addendum: addendum_4.clone(),
                include_tx: Boolean::True,
            }),
            source: ServiceId::Syncer(Coin::Monero),
        })
        .unwrap();
    }

    for _ in 0..5 {
        println!("waiting for repeated watch transaction message");
        let message = rx_event.recv_multipart(0).unwrap();
        println!("received repeated transaction message");
        let request = get_request_from_message(message);
        assert_address_transaction(request, 1, vec![tx_id4.clone()]);
    }
}

/*
We test for the following scenarios in the transaction tests:

- Submit a WatchTransaction task for a transaction in the mempool, receive confirmation events until
the threshold confs are reached

- Submit a WatchTransaction task for a mined transaction, receive confirmation events

- Submit two WatchTransaction tasks in parallel, receive confirmation events for both
*/
async fn monero_syncer_transaction_test() {
    let (regtest, wallet) = setup_monero().await;
    let address = wallet.get_address(0, None).await.unwrap().address;
    let blocks = regtest.generate_blocks(200, address).await.unwrap();

    // allow some time for things to happen, like the wallet server catching up
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);

    // create a monero syncer
    let (tx, rx_event) = create_monero_syncer();

    let txid_1 = send_monero(&wallet, address, 1).await;

    tx.send(SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: 1,
            lifetime: blocks + 5,
            hash: txid_1.to_vec(),
            confirmation_bound: 2,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    })
    .unwrap();

    println!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, 0, vec![0]);

    let block_height = regtest.generate_blocks(1, address).await.unwrap();
    let block_hash = get_block_hash_from_height(&regtest, block_height).await;
    println!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, 1, block_hash.clone());

    regtest.generate_blocks(1, address).await.unwrap();
    println!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, 2, block_hash);

    let tx_id2 = send_monero(&wallet, address, 1).await;
    let block_height = regtest.generate_blocks(1, address).await.unwrap();
    let block_hash = get_block_hash_from_height(&regtest, block_height).await;
    tx.send(SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: 1,
            lifetime: block_height + 5,
            hash: tx_id2,
            confirmation_bound: 2,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    })
    .unwrap();
    println!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, 1, block_hash.clone());

    regtest.generate_blocks(1, address).await.unwrap();
    println!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, 2, block_hash);

    let txid_2 = send_monero(&wallet, address, 1).await;
    let txid_3 = send_monero(&wallet, address, 1).await;

    tx.send(SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: 1,
            lifetime: blocks + 5,
            hash: txid_2.to_vec(),
            confirmation_bound: 2,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    })
    .unwrap();
    tx.send(SyncerdTask {
        task: Task::WatchTransaction(WatchTransaction {
            id: 1,
            lifetime: blocks + 5,
            hash: txid_3.to_vec(),
            confirmation_bound: 2,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    })
    .unwrap();

    println!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, 0, vec![0]);
    println!("awaiting confirmations");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("received confirmation");
    let request = get_request_from_message(message);
    assert_transaction_confirmations(request, 0, vec![0]);
}

/*
Check that a monero BroadcastTransaction task generates an error
*/
async fn monero_syncer_broadcast_tx_test() {
    let (regtest, wallet) = setup_monero().await;
    let address = wallet.get_address(0, None).await.unwrap();
    regtest.generate_blocks(1, address.address).await.unwrap();

    let (tx, rx_event) = create_monero_syncer();

    let task = SyncerdTask {
        task: Task::BroadcastTransaction(BroadcastTransaction { id: 0, tx: vec![0] }),
        source: ServiceId::Syncer(Coin::Monero),
    };
    tx.send(task).unwrap();

    println!("waiting for transaction broadcasted message");
    let message = rx_event.recv_multipart(0).unwrap();
    println!("transaction broadcasted");
    let request = get_request_from_message(message);
    assert_transaction_broadcasted(
        request,
        true,
        Some("broadcast transaction not available for Monero".to_string()),
    );
}

async fn setup_monero() -> (monero_rpc::RegtestDaemonClient, monero_rpc::WalletClient) {
    let daemon_client = monero_rpc::RpcClient::new("http://localhost:18081".to_string());
    let daemon = daemon_client.daemon();
    let regtest = daemon.regtest();
    let wallet_client = monero_rpc::RpcClient::new("http://localhost:18083".to_string());
    let wallet = wallet_client.wallet();
    match wallet
        .create_wallet("test".to_string(), None, "English".to_string())
        .await
    {
        _ => {
            wallet.open_wallet("test".to_string(), None).await.unwrap();
        }
    }
    (regtest, wallet)
}

fn create_monero_syncer() -> (std::sync::mpsc::Sender<SyncerdTask>, zmq::Socket) {
    let (tx, rx): (Sender<SyncerdTask>, Receiver<SyncerdTask>) = std::sync::mpsc::channel();
    let tx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
    let rx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
    tx_event.connect("inproc://testmonerobridge").unwrap();
    rx_event.bind("inproc://testmonerobridge").unwrap();
    let mut syncer = MoneroSyncer::new();
    let syncer_servers = SyncerServers {
        electrum_server: "".to_string(),
        monero_daemon: "http://localhost:18081".to_string(),
        monero_rpc_wallet: "http://localhost:18084".to_string(),
    };
    let dummy = BlockHash::from_slice(&[0; 32]).unwrap();
    syncer.run(
        rx,
        tx_event,
        ServiceId::Syncer(Coin::Monero).into(),
        syncer_servers,
        Chain::Regtest(dummy),
        true,
    );
    (tx, rx_event)
}

async fn send_monero(
    wallet: &monero_rpc::WalletClient,
    address: monero::Address,
    amount: u64,
) -> Vec<u8> {
    let mut destination: HashMap<Address, u64> = HashMap::new();
    destination.insert(address, amount);
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
            destination.clone(),
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
    let request = (&*unmarshaller.unmarshall(&plain_message).unwrap()).clone();
    request
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

fn assert_address_transaction(request: Request, expected_amount: u64, expected_txid: Vec<Vec<u8>>) {
    match request {
        Request::SyncerdBridgeEvent(event) => match event.event {
            Event::AddressTransaction(address_transaction) => {
                assert_eq!(address_transaction.amount, expected_amount);
                assert!(expected_txid.contains(&address_transaction.hash));
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
    expected_confirmations: u32,
    expected_block_hash: Vec<u8>,
) {
    match request {
        Request::SyncerdBridgeEvent(event) => match event.event {
            Event::TransactionConfirmations(transaction_confirmations) => {
                assert_eq!(
                    transaction_confirmations.confirmations,
                    Some(expected_confirmations)
                );
                assert_eq!(transaction_confirmations.block, expected_block_hash);
            }
            _ => panic!("expected address transaction event"),
        },
        _ => panic!("expected syncerd bridge event"),
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
