#[macro_use]
extern crate log;

use crate::farcaster::farcaster_client::FarcasterClient;
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
    let (farcasterd_maker, _, farcasterd_taker, _) = setup_clients().await;

    // Allow some time for the microservices to start and register each other
    tokio::time::sleep(time::Duration::from_secs(10)).await;

    let channel = Endpoint::from_static("http://[::1]:50051")
        .connect()
        .await
        .unwrap();

    let mut farcaster_client = FarcasterClient::new(channel.clone());
    let request = tonic::Request::new(InfoRequest { id: 0 });
    let response = farcaster_client.info(request).await;
    assert_eq!(response.unwrap().into_inner().id, 0);
    cleanup_processes(vec![farcasterd_maker, farcasterd_taker]);
}
