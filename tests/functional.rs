use bitcoincore_rpc::{Auth, Client, RpcApi};
use farcaster_core::bitcoin::tasks::BtcAddressAddendum;
use farcaster_core::syncer::Event;
use farcaster_core::syncer::Task;
use farcaster_core::syncer::WatchAddress;
use farcaster_core::syncer::WatchHeight;
use farcaster_node::rpc::Request;
use farcaster_node::syncerd::bitcoin_syncer::BitcoinSyncer;
use farcaster_node::syncerd::bitcoin_syncer::Synclet;
use farcaster_node::syncerd::opts::Coin;
use farcaster_node::syncerd::runtime::SyncerdTask;
use farcaster_node::syncerd::SyncerServers;
use farcaster_node::ServiceId;
use internet2::transport::MAX_FRAME_SIZE;
use internet2::Decrypt;
use internet2::PlainTranscoder;
use internet2::RoutedFrame;
use internet2::ZMQ_CONTEXT;
use monero::Address;
use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;

use bitcoin::hashes::Hash;
use farcaster_core::consensus;
use internet2::{CreateUnmarshaller, Unmarshall};
use std::str::FromStr;

#[test]
fn bitcoin_syncer_block_height_test() {
    let path =
        std::path::PathBuf::from_str("/home/drgrid/containers/data_dir/regtest/.cookie").unwrap();
    let bitcoin_rpc =
        Client::new("http://localhost:18443".to_string(), Auth::CookieFile(path)).unwrap();

    // make sure a wallet is created and loaded
    match bitcoin_rpc.create_wallet("wallet", None, None, None, None) {
        Err(_e) => match bitcoin_rpc.load_wallet("wallet") {
            _ => {}
        },
        _ => {}
    }

    // generate some blocks to an address
    let address = bitcoin_rpc.get_new_address(None, None).unwrap();
    // Generate over 101 blocks to reach block maturity, and some more for extra leeway
    bitcoin_rpc.generate_to_address(110, &address).unwrap();

    // start a bitcoin syncer
    let (tx, rx): (Sender<SyncerdTask>, Receiver<SyncerdTask>) = std::sync::mpsc::channel();
    let tx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
    let rx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
    tx_event.connect("inproc://syncerdbridge").unwrap();
    rx_event.bind("inproc://syncerdbridge").unwrap();
    let mut syncer = BitcoinSyncer::new();
    let syncer_servers = SyncerServers {
        electrum_server: "tcp://localhost:50001".to_string(),
        monero_daemon: "".to_string(),
        monero_rpc_wallet: "".to_string(),
    };

    // allow some time for things to happen, like electrum server catching up
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);

    syncer.run(
        rx,
        tx_event,
        ServiceId::Syncer(Coin::Bitcoin).into(),
        syncer_servers,
    );

    let blocks = bitcoin_rpc.get_block_count().unwrap();

    // Send a WatchHeight task
    let task = SyncerdTask {
        task: Task::WatchHeight(WatchHeight {
            id: 0,
            lifetime: blocks + 2,
            addendum: vec![],
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    };
    tx.send(task).unwrap();

    // Receive the request and compare it to the actual block count
    let message = rx_event.recv_multipart(0).unwrap();
    let request = get_request_from_message(message);
    assert_received_height_changed(request, blocks);
    // Generate a single height changed event
    bitcoin_rpc.generate_to_address(1, &address).unwrap();
    let message = rx_event.recv_multipart(0).unwrap();
    let request = get_request_from_message(message);
    let blocks = bitcoin_rpc.get_block_count().unwrap();
    assert_received_height_changed(request, blocks);

    // Send another WatchHeight task
    let task = SyncerdTask {
        task: Task::WatchHeight(WatchHeight {
            id: 1,
            lifetime: blocks + 2,
            addendum: vec![],
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    };
    tx.send(task).unwrap();
    let message = rx_event.recv_multipart(0).unwrap();
    let request = get_request_from_message(message);
    assert_received_height_changed(request, blocks);

    // generate another block - this should result in two height changed messages
    bitcoin_rpc.generate_to_address(1, &address).unwrap();
    let message = rx_event.recv_multipart(0).unwrap();
    let request = get_request_from_message(message);
    let blocks = bitcoin_rpc.get_block_count().unwrap();
    assert_received_height_changed(request, blocks);
    let message = rx_event.recv_multipart(0).unwrap();
    let request = get_request_from_message(message);
    let blocks = bitcoin_rpc.get_block_count().unwrap();
    assert_received_height_changed(request, blocks);
}

#[test]
fn bitcoin_syncer_address_test() {
    let path =
        std::path::PathBuf::from_str("/home/drgrid/containers/data_dir/regtest/.cookie").unwrap();
    let bitcoin_rpc =
        Client::new("http://localhost:18443".to_string(), Auth::CookieFile(path)).unwrap();

    // make sure a wallet is created and loaded
    match bitcoin_rpc.create_wallet("wallet", None, None, None, None) {
        Err(_e) => match bitcoin_rpc.load_wallet("wallet") {
            _ => {}
        },
        _ => {}
    }

    // generate some blocks to an address
    let address = bitcoin_rpc.get_new_address(None, None).unwrap();
    let amount = bitcoin::Amount::ONE_BTC;
    // Generate over 101 blocks to reach block maturity, and some more for extra leeway
    bitcoin_rpc.generate_to_address(110, &address).unwrap();

    // start a bitcoin syncer
    let (tx, rx): (Sender<SyncerdTask>, Receiver<SyncerdTask>) = std::sync::mpsc::channel();
    let tx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
    let rx_event = ZMQ_CONTEXT.socket(zmq::PAIR).unwrap();
    tx_event.connect("inproc://syncerdbridge").unwrap();
    rx_event.bind("inproc://syncerdbridge").unwrap();
    let mut syncer = BitcoinSyncer::new();
    let syncer_servers = SyncerServers {
        electrum_server: "tcp://localhost:50001".to_string(),
        monero_daemon: "".to_string(),
        monero_rpc_wallet: "".to_string(),
    };

    // allow some time for things to happen, e.g. electrum server to catch up
    let duration = std::time::Duration::from_secs(10);
    std::thread::sleep(duration);

    syncer.run(
        rx,
        tx_event,
        ServiceId::Syncer(Coin::Bitcoin).into(),
        syncer_servers,
    );

    let blocks = bitcoin_rpc.get_block_count().unwrap();

    // Generate two addresses and watch them
    let address1 = bitcoin_rpc.get_new_address(None, None).unwrap();
    let address2 = bitcoin_rpc.get_new_address(None, None).unwrap();

    let addendum_1 = BtcAddressAddendum {
        address: address1.to_string(),
        from_height: 0,
        script_pubkey: address1.script_pubkey().to_bytes(),
    };
    let addendum_2 = BtcAddressAddendum {
        address: address2.to_string(),
        from_height: 0,
        script_pubkey: address2.script_pubkey().to_bytes(),
    };
    let watch_address_task = SyncerdTask {
        task: Task::WatchAddress(WatchAddress {
            id: 1,
            lifetime: blocks + 1,
            addendum: consensus::serialize(&addendum_1),
            include_tx: farcaster_core::syncer::Boolean::True,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    };
    tx.send(watch_address_task).unwrap();
    let watch_address_task_2 = SyncerdTask {
        task: Task::WatchAddress(WatchAddress {
            id: 1,
            lifetime: blocks + 2,
            addendum: consensus::serialize(&addendum_2),
            include_tx: farcaster_core::syncer::Boolean::True,
        }),
        source: ServiceId::Syncer(Coin::Bitcoin),
    };
    tx.send(watch_address_task_2).unwrap();

    // send some coins to address3
    bitcoin_rpc
        .send_to_address(&address1, amount, None, None, None, None, None, None)
        .unwrap();
    println!("waiting for watch transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    let request = get_request_from_message(message);
    assert_address_transaction(request, 100000000, vec![]);

    // now generate a block for that address, wait for the response and test it
    let block_hash = bitcoin_rpc.generate_to_address(1, &address1).unwrap();

    println!("waiting for watch transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    let request = get_request_from_message(message);
    let block = bitcoin_rpc.get_block(&block_hash[0]).unwrap();
    let address_transaction_amount = find_coinbase_transaction_amount(block.txdata);
    assert_address_transaction(request, address_transaction_amount, block_hash[0].to_vec());

    // then send a transaction to the other address we are watching
    bitcoin_rpc
        .send_to_address(&address2, amount, None, None, None, None, None, None)
        .unwrap();
    println!("waiting for watch transaction message");
    let message = rx_event.recv_multipart(0).unwrap();
    let request = get_request_from_message(message);
    assert_address_transaction(request, 100000000, vec![]);
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

#[tokio::test]
async fn functional_monero_syncer_test() {
    let daemon_client = monero_rpc::RpcClient::new("http://localhost:18081".to_string());
    let daemon = daemon_client.daemon();
    let regtest = daemon.regtest();
    let count = regtest.get_block_count().await.unwrap();
    println!("count: {:?}", count);

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

    let address = wallet.get_address(0, None).await.unwrap();
    let generate = regtest.generate_blocks(100, address.address).await.unwrap();
    println!("generated: {:?}", generate);

    let balance = wallet.get_balance(0, None).await.unwrap();
    println!("balance: {:?}", balance);

    let mut destination: HashMap<Address, u64> = HashMap::new();
    destination.insert(address.address, 1);

    let options = monero_rpc::TransferOptions {
        account_index: None,
        subaddr_indices: None,
        mixin: None,
        ring_size: None,
        unlock_time: None,
        payment_id: None,
        do_not_relay: None,
    };

    wallet
        .transfer(destination, monero_rpc::TransferPriority::Default, options)
        .await
        .unwrap();
}

fn assert_address_transaction(
    request: Request,
    expected_amount: u64,
    expected_block_hash: Vec<u8>,
) {
    match request {
        Request::SyncerdBridgeEvent(event) => match event.event {
            Event::AddressTransaction(address_transaction) => {
                assert_eq!(address_transaction.amount, expected_amount);
                assert_eq!(address_transaction.block, expected_block_hash);
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
