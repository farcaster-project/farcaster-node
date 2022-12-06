#[macro_use]
extern crate log;

use crate::farcaster::{
    farcaster_client::FarcasterClient, AbortSwapRequest, CheckpointSelector, CheckpointsRequest,
    InfoResponse, ListOffersRequest, MakeRequest, NeedsFundingRequest, OfferInfoRequest,
    OfferSelector, PeersRequest, ProgressRequest, RestoreCheckpointRequest, RevokeOfferRequest,
    SwapInfoRequest, SweepAddressRequest, TakeRequest,
};
use bitcoincore_rpc::RpcApi;
use farcaster::{InfoRequest, MakeResponse, NeedsFundingResponse};
use std::{str::FromStr, sync::Arc, time};
use tonic::transport::Endpoint;
use utils::{config, fc::*};

mod utils;

pub mod farcaster {
    tonic::include_proto!("farcaster");
}

const ALLOWED_RETRIES: u32 = 100;

#[tokio::test]
#[ignore]
async fn grpc_server_functional_test() {
    let conf = config::TestConfig::parse();
    let _ = launch_farcasterd_pair().await;

    // Allow some time for the microservices to start and register each other
    tokio::time::sleep(time::Duration::from_secs(10)).await;

    let grpc_1 = conf.grpc.fc1.to_string();
    let channel_1 = Endpoint::from_str(&grpc_1)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut farcaster_client_1 = FarcasterClient::new(channel_1);

    let channel_2 = Endpoint::from_str(&conf.grpc.fc2.to_string())
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut farcaster_client_2 = FarcasterClient::new(channel_2);

    // Test get info
    let request = tonic::Request::new(InfoRequest { id: 0 });
    let response = farcaster_client_1.info(request).await;
    assert_eq!(response.unwrap().into_inner().id, 0);

    // Test list peers
    let request = tonic::Request::new(PeersRequest { id: 1 });
    let response = farcaster_client_1.peers(request).await;
    assert_eq!(response.unwrap().into_inner().id, 1);

    // Test list checkpoints
    let request = tonic::Request::new(CheckpointsRequest {
        id: 2,
        selector: CheckpointSelector::AllCheckpoints.into(),
    });
    let response = farcaster_client_1.checkpoints(request).await;
    assert_eq!(response.unwrap().into_inner().id, 2);

    let bitcoin_rpc = Arc::new(bitcoin_setup());
    let (_monero_regtest, monero_wallet) = monero_setup().await;

    let (xmr_address, _xmr_address_wallet_name) =
        monero_new_dest_address(Arc::clone(&monero_wallet)).await;
    let btc_address = bitcoin_rpc.get_new_address(None, None).unwrap();

    // Test make offer
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
        public_port: 7067,
    };
    let request = tonic::Request::new(make_request.clone());
    let response = farcaster_client_1.make(request).await;
    let MakeResponse { id, offer } = response.unwrap().into_inner();
    assert_eq!(id, 3);

    // Test revoke offer
    let request = tonic::Request::new(RevokeOfferRequest {
        id: 4,
        public_offer: offer.to_string(),
    });
    let response = farcaster_client_1.revoke_offer(request).await;
    assert_eq!(response.unwrap().into_inner().id, 4);

    // Test make another offer
    let request = tonic::Request::new(make_request.clone());
    let response = farcaster_client_1.make(request).await;
    let MakeResponse { id, offer } = response.unwrap().into_inner();
    assert_eq!(id, 3);

    let (xmr_address, _xmr_address_wallet_name) =
        monero_new_dest_address(Arc::clone(&monero_wallet)).await;
    let btc_address = bitcoin_rpc.get_new_address(None, None).unwrap();

    // Test Offer info
    let offer_info_request = tonic::Request::new(OfferInfoRequest {
        id: 21,
        public_offer: offer.clone().to_string(),
    });
    let response = farcaster_client_2.offer_info(offer_info_request).await;
    assert_eq!(response.unwrap().into_inner().id, 21);

    // Test List offers
    let list_offers_request = tonic::Request::new(ListOffersRequest {
        id: 22,
        selector: OfferSelector::All.into(),
    });
    let response = farcaster_client_1.list_offers(list_offers_request).await;
    assert_eq!(response.unwrap().into_inner().id, 22);

    // Test take offer
    let take_request = TakeRequest {
        id: 5,
        public_offer: offer.to_string(),
        bitcoin_address: btc_address.to_string(),
        monero_address: xmr_address.to_string(),
    };
    let request = tonic::Request::new(take_request.clone());
    let response = farcaster_client_2.take(request).await;
    assert_eq!(response.unwrap().into_inner().id, 5);

    let swap_id = retry_until_swap_id(&mut farcaster_client_2).await;

    tokio::time::sleep(time::Duration::from_secs(5)).await;

    // Test progress
    let request = tonic::Request::new(ProgressRequest {
        id: 10,
        swap_id: swap_id.clone(),
    });
    let response = farcaster_client_2.progress(request).await;
    assert_eq!(response.unwrap().into_inner().id, 10);

    tokio::time::sleep(time::Duration::from_secs(5)).await;

    // Test needs funding
    let (address, amount) = retry_until_bitcoin_funding_info(&mut farcaster_client_1).await;

    bitcoin_rpc
        .send_to_address(&address, amount, None, None, None, None, None, None)
        .unwrap();

    // Test abort swap
    let request = tonic::Request::new(AbortSwapRequest { id: 12, swap_id });
    let response = farcaster_client_1.abort_swap(request).await;
    assert_eq!(response.unwrap().into_inner().id, 12);

    kill_all();

    let _ = launch_farcasterd_pair().await;

    tokio::time::sleep(time::Duration::from_secs(5)).await;

    // Test sweep address on re-launch
    let btc_address = bitcoin_rpc.get_new_address(None, None).unwrap();
    let channel_1 = Endpoint::from_str(&grpc_1)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut farcaster_client_1 = FarcasterClient::new(channel_1);
    let request = tonic::Request::new(SweepAddressRequest {
        id: 13,
        source_address: address.to_string(),
        destination_address: btc_address.to_string(),
    });
    let response = farcaster_client_1.sweep_address(request).await;
    assert_eq!(response.unwrap().into_inner().id, 13);

    // Test restore checkpoint
    let request = tonic::Request::new(make_request.clone());
    let response = farcaster_client_1.make(request).await;
    let MakeResponse { offer, .. } = response.unwrap().into_inner();
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

    let swap_id = retry_until_swap_id(&mut farcaster_client_2).await;

    // wait for funding
    tokio::time::sleep(time::Duration::from_secs(5)).await;
    let (address, amount) = retry_until_bitcoin_funding_info(&mut farcaster_client_1).await;

    bitcoin_rpc
        .send_to_address(&address, amount, None, None, None, None, None, None)
        .unwrap();

    // test swap info
    let request = tonic::Request::new(SwapInfoRequest {
        id: 20,
        swap_id: swap_id.clone(),
    });
    let response = farcaster_client_1.swap_info(request).await;
    assert_eq!(response.unwrap().into_inner().id, 20);
    // wait for lock
    tokio::time::sleep(time::Duration::from_secs(15)).await;
    kill_all();
    let _ = launch_farcasterd_pair().await;
    tokio::time::sleep(time::Duration::from_secs(5)).await;
    let channel_1 = Endpoint::from_str(&grpc_1)
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut farcaster_client_1 = FarcasterClient::new(channel_1);
    let request = tonic::Request::new(RestoreCheckpointRequest { id: 12, swap_id });
    let response = farcaster_client_1.restore_checkpoint(request).await;
    assert_eq!(response.unwrap().into_inner().id, 12);

    kill_all();
}

async fn retry_until_swap_id(client: &mut FarcasterClient<tonic::transport::Channel>) -> String {
    for _ in 0..ALLOWED_RETRIES {
        let request = tonic::Request::new(InfoRequest { id: 6 });
        let InfoResponse { swaps, .. } = client.info(request).await.unwrap().into_inner();
        if !swaps.is_empty() {
            return swaps[0].clone();
        }
        tokio::time::sleep(time::Duration::from_secs(1)).await;
    }
    panic!("timeout before a swap id could be retrieved")
}

async fn retry_until_bitcoin_funding_info(
    client: &mut FarcasterClient<tonic::transport::Channel>,
) -> (bitcoin::Address, bitcoin::Amount) {
    for _ in 0..ALLOWED_RETRIES {
        let request = tonic::Request::new(NeedsFundingRequest {
            id: 11,
            blockchain: farcaster::Blockchain::Bitcoin.into(),
        });
        let response = client.needs_funding(request).await;
        let NeedsFundingResponse { id, funding_infos } = response.unwrap().into_inner();
        if !funding_infos.is_empty() {
            return (
                bitcoin::Address::from_str(&funding_infos[0].address).unwrap(),
                bitcoin::Amount::from_sat(funding_infos[0].amount),
            );
        }
        tokio::time::sleep(time::Duration::from_secs(1)).await;
    }
    panic!("timeout before funding info could be retrieved")
}
