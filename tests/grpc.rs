#[macro_use]
extern crate log;

use crate::farcaster::{
    farcaster_client::FarcasterClient, CheckpointsRequest, MakeRequest, PeersRequest,
    RevokeOfferRequest,
};
use bitcoincore_rpc::RpcApi;
use farcaster::InfoRequest;
use std::{str::FromStr, sync::Arc, time};
use tonic::transport::Endpoint;
use utils::fc::*;

mod utils;

pub mod farcaster {
    tonic::include_proto!("farcaster");
}

#[tokio::test]
#[ignore]
async fn grpc_server_functional_test() {
    let _ = setup_clients().await;

    // Allow some time for the microservices to start and register each other
    tokio::time::sleep(time::Duration::from_secs(10)).await;

    let channel = Endpoint::from_static("http://0.0.0.0:23432")
        .connect()
        .await
        .unwrap();

    let mut farcaster_client = FarcasterClient::new(channel.clone());
    let request = tonic::Request::new(InfoRequest { id: 0 });
    let response = farcaster_client.info(request).await;
    assert_eq!(response.unwrap().into_inner().id, 0);

    let request = tonic::Request::new(PeersRequest { id: 1 });
    let response = farcaster_client.peers(request).await;
    assert_eq!(response.unwrap().into_inner().id, 1);

    let request = tonic::Request::new(CheckpointsRequest { id: 2 });
    let response = farcaster_client.checkpoints(request).await;
    println!("response: {:?}", response);
    assert_eq!(response.unwrap().into_inner().id, 2);

    let bitcoin_rpc = Arc::new(bitcoin_setup());
    let (_monero_regtest, monero_wallet) = monero_setup().await;

    let (xmr_address, _xmr_address_wallet_name) =
        monero_new_dest_address(Arc::clone(&monero_wallet)).await;
    let btc_address = bitcoin_rpc.get_new_address(None, None).unwrap();

    let request = tonic::Request::new(MakeRequest {
        id: 3,
        network: farcaster::Network::Local.into(),
        arbitrating_blockchain: farcaster::Blockchain::Bitcoin.into(),
        accordant_blockchain: farcaster::Blockchain::Monero.into(),
        arbitrating_amount: bitcoin::Amount::from_str("1 BTC").unwrap().as_sat(),
        accordant_amount: monero::Amount::from_str("1 XMR").unwrap().as_pico(),
        arbitrating_addr: btc_address.to_string(),
        accordant_addr: xmr_address.to_string(),
        cancel_timelock: 20,
        punish_timelock: 40,
        fee_strategy: "100 satoshi/vByte".to_string(),
        maker_role: farcaster::SwapRole::Bob.into(),
        public_ip_addr: "127.0.0.1".to_string(),
        bind_ip_addr: "0.0.0.0".to_string(),
        port: 9376,
    });

    let response = farcaster_client.make(request).await;
    println!("response: {:?}", response);
    let offer = response.unwrap().into_inner().offer;

    let request = tonic::Request::new(RevokeOfferRequest {
        id: 4,
        public_offer: offer.to_string(),
    });
    let response = farcaster_client.revoke_offer(request).await;
    println!("response: {:?}", response);

    kill_all();
}
