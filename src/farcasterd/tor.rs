use bitcoin::secp256k1::SecretKey;
use internet2::addr::InetAddr;
use reqwest;
use torut::onion::OnionAddressV3;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tokio::net::TcpStream;
use torut::control::{TorAuthData, TorAuthMethod, UnauthenticatedConn};
use torut::utils::{run_tor, AutoKillChild};

/// checks if tor is running
pub async fn assert_tor_running() {
    // Make sure you are running tor and this is your socks port
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all("socks5://127.0.0.1:9050".to_string()).unwrap())
        .build()
        .unwrap();

    let res = client.get("https://check.torproject.org").send().await.unwrap();
    let text = res.text().await.unwrap();
    if !text.contains("Congratulations. This browser is configured to use Tor.") {
        panic!("Tor is currently not running")
    }
    println!("tor is indeed running");
}

async fn create_v3_onion_service() -> InetAddr {
    // Check that a Tor instance is running
    assert_tor_running().await;

    // Connect to the Tor control socket
    let s = TcpStream::connect(&format!("127.0.0.1:{}", 9051))
        .await
        .unwrap();
    let mut uc = UnauthenticatedConn::new(s);

    // Authenticate to the tor socket
    let proto_info = uc.load_protocol_info().await.unwrap();
    let tor_auth_data = proto_info
        .make_auth_data()
        .expect("Failed to make Tor auth data")
        .unwrap();
    uc.authenticate(&tor_auth_data)
        .await
        .expect("cannot authenticate");
    let mut ac = uc.into_authenticated().await;

    // We have to set a dummy event handler, even if we are not reading any
    // events
    ac.set_async_event_handler(Some(|_| async move { Ok(()) }));

    // Take ownership (meaning control) the Tor instance
    ac.take_ownership().await.unwrap();

    // Generate a secret service
    let key = torut::onion::TorSecretKeyV3::generate();
    ac.add_onion_v3(
        &key,
        false,
        false,
        false,
        None,
        &mut [(
            15787,
            SocketAddr::new(IpAddr::from(Ipv4Addr::new(127, 0, 0, 1)), 15787),
        )]
        .iter(),
    )
    .await
    .unwrap();
    let addr = InetAddr::Tor(key.public());

    // We have to rescind ownership (control) of the connection in
    // order for Tor to not exit when the control socket is closed
    ac.drop_ownership().await.unwrap();

    addr
}

#[tokio::test]
async fn test_this_onion() {
    create_v3_onion_service().await;
}
