#[macro_use]
extern crate log;

use crate::farcaster::{
    farcaster_client::FarcasterClient, AbortSwapRequest, CheckpointsRequest, InfoResponse,
    MakeRequest, NeedsFundingRequest, PeersRequest, ProgressRequest, RevokeOfferRequest,
    SweepAddressRequest, TakeRequest,
};
use bitcoincore_rpc::RpcApi;
use farcaster::{InfoRequest, MakeResponse, NeedsFundingResponse};
use farcaster_node::bus::ctl::BitcoinFundingInfo;
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

    let channel_1 = Endpoint::from_static("http://0.0.0.0:23432")
        .connect()
        .await
        .unwrap();
    let mut farcaster_client_1 = FarcasterClient::new(channel_1);

    let channel_2 = Endpoint::from_static("http://0.0.0.0:23433")
        .connect()
        .await
        .unwrap();
    let mut farcaster_client_2 = FarcasterClient::new(channel_2);

    let request = tonic::Request::new(InfoRequest { id: 0 });
    let response = farcaster_client_1.info(request).await;
    assert_eq!(response.unwrap().into_inner().id, 0);

    let request = tonic::Request::new(PeersRequest { id: 1 });
    let response = farcaster_client_1.peers(request).await;
    assert_eq!(response.unwrap().into_inner().id, 1);

    let request = tonic::Request::new(CheckpointsRequest { id: 2 });
    let response = farcaster_client_1.checkpoints(request).await;
    println!("response: {:?}", response);
    assert_eq!(response.unwrap().into_inner().id, 2);

    let bitcoin_rpc = Arc::new(bitcoin_setup());
    let (_monero_regtest, monero_wallet) = monero_setup().await;

    let (xmr_address, _xmr_address_wallet_name) =
        monero_new_dest_address(Arc::clone(&monero_wallet)).await;
    let btc_address = bitcoin_rpc.get_new_address(None, None).unwrap();

    let make_request = MakeRequest {
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
    };
    let request = tonic::Request::new(make_request.clone());
    let response = farcaster_client_1.make(request).await;
    let MakeResponse { id, offer } = response.unwrap().into_inner();
    assert_eq!(id, 3);

    let request = tonic::Request::new(RevokeOfferRequest {
        id: 4,
        public_offer: offer.to_string(),
    });
    let response = farcaster_client_1.revoke_offer(request).await;
    assert_eq!(response.unwrap().into_inner().id, 4);

    // make another offer
    let request = tonic::Request::new(make_request.clone());
    let response = farcaster_client_1.make(request).await;
    let MakeResponse { id, offer } = response.unwrap().into_inner();
    assert_eq!(id, 3);

    let (xmr_address, _xmr_address_wallet_name) =
        monero_new_dest_address(Arc::clone(&monero_wallet)).await;
    let btc_address = bitcoin_rpc.get_new_address(None, None).unwrap();

    let take_request = TakeRequest {
        id: 5,
        public_offer: offer.to_string(),
        bitcoin_address: btc_address.to_string(),
        monero_address: xmr_address.to_string(),
    };
    let request = tonic::Request::new(take_request.clone());
    let response = farcaster_client_2.take(request).await;
    assert_eq!(response.unwrap().into_inner().id, 5);

    tokio::time::sleep(time::Duration::from_secs(5)).await;

    let request = tonic::Request::new(InfoRequest { id: 6 });
    let InfoResponse { swaps, .. } = farcaster_client_2.info(request).await.unwrap().into_inner();
    let swap_id = swaps[0].clone();

    tokio::time::sleep(time::Duration::from_secs(5)).await;

    let request = tonic::Request::new(ProgressRequest {
        id: 10,
        swap_id: swap_id.clone(),
    });
    let response = farcaster_client_2.progress(request).await;
    println!("response: {:?}", response);
    assert_eq!(response.unwrap().into_inner().id, 10);

    tokio::time::sleep(time::Duration::from_secs(5)).await;

    let request = tonic::Request::new(NeedsFundingRequest {
        id: 11,
        blockchain: farcaster::Blockchain::Bitcoin.into(),
    });
    let response = farcaster_client_1.needs_funding(request).await;
    println!("response: {:?}", response);
    let NeedsFundingResponse { id, funding_infos } = response.unwrap().into_inner();
    assert_eq!(id, 11);

    let funding_info = BitcoinFundingInfo::from_str(&funding_infos).unwrap();
    bitcoin_rpc
        .send_to_address(
            &funding_info.address,
            funding_info.amount,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

    // tokio::time::sleep(time::Duration::from_secs(5)).await;
    let request = tonic::Request::new(AbortSwapRequest { id: 12, swap_id });
    let response = farcaster_client_1.abort_swap(request).await;
    println!("response: {:?}", response);
    assert_eq!(response.unwrap().into_inner().id, 12);

    kill_all();

    let _ = setup_clients().await;
    let btc_address = bitcoin_rpc.get_new_address(None, None).unwrap();
    let channel_1 = Endpoint::from_static("http://0.0.0.0:23432")
        .connect()
        .await
        .unwrap();
    let mut farcaster_client_1 = FarcasterClient::new(channel_1);
    let request = tonic::Request::new(SweepAddressRequest {
        id: 13,
        source_address: funding_info.address.to_string(),
        destination_address: btc_address.to_string(),
    });
    let response = farcaster_client_1.sweep_address(request).await;
    println!("response: {:?}", response);
    assert_eq!(response.unwrap().into_inner().id, 13);
}
