#[macro_use]
extern crate log;

use crate::farcaster::{farcaster_client::FarcasterClient, CheckpointsRequest, PeersRequest};
use farcaster::InfoRequest;
use std::time;
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

    kill_all();
}
