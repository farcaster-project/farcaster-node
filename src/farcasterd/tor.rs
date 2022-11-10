use bitcoin::secp256k1::SecretKey;
use bitcoin::secp256k1::ffi::secp256k1_context_create;
use internet2::addr::{InetAddr, InetSocketAddr, LocalNode};
use internet2::session::BrontozaurSession;
use reqwest;
use socks::{Socks5Stream, TargetAddr};
use torut::onion::{OnionAddressV3, TorPublicKey, TorPublicKeyV3};
use std::io::{Write, Read};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::str::FromStr;
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

async fn create_v3_onion_service() -> InetSocketAddr {
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
            SocketAddr::new(IpAddr::from(Ipv4Addr::new(0, 0, 0, 0)), 12345),
        )]
        .iter(),
    )
    .await
    .unwrap();
    println!("public key: {}", key.public().get_onion_address());
    let inet_socket_addr = InetSocketAddr::Tor(key.public());

    // We have to rescind ownership (control) of the connection in
    // order for Tor to not exit when the control socket is closed
    ac.drop_ownership().await.unwrap();

    inet_socket_addr
}

#[tokio::test]
async fn test_this_onion() {
    // let inet_socket_addr = create_v3_onion_service().await;
    // !!! Configure RUST_LOG in CI to change this value !!!
    // set default level to info, debug for node
    // info level is for bitcoin and monero that correspond to `tests/{bitcoin.rs,monero.rs}`
    let env = env_logger::Env::new().default_filter_or("info,farcaster_node=debug");
    // try init the logger, this fails if logger has been already initialized
    // and it happens when multiple tests are called as the (test)binary is not restarted
    let _ = env_logger::from_env(env).is_test(true).try_init();

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
        true, // set detach to true to let the onion service continue to run on tor
        false,
        false,
        None,
        &mut [(
            15787,
            SocketAddr::new(IpAddr::from(Ipv4Addr::new(0, 0, 0, 0)), 12345),
        )]
        .iter(),
    )
    .await
    .unwrap();
    println!("public key: {}", key.public().get_onion_address());
    let inet_socket_addr = InetSocketAddr::Tor(key.public());

    // We have to rescind ownership (control) of the connection in
    // order for Tor to not exit when the control socket is closed
    ac.drop_ownership().await.unwrap();


    // assert_tor_running();
    let onion_address = if let InetSocketAddr::Tor(addr) = inet_socket_addr {
        Some(addr.get_onion_address())
    } else {
        None
    }.unwrap();

    std::thread::spawn(|| {
        let listener = TcpListener::bind(SocketAddr::from_str("0.0.0.0:12345").unwrap()).unwrap();
        println!("Awaiting for incoming connections...");
        let (mut stream, remote_socket_addr) = listener
            .accept()
            .expect("Error accepting incoming peer connection");
        println!("New connection from {}", remote_socket_addr);
        let local_node = LocalNode::new(bitcoin::secp256k1::SECP256K1);

        let inet_addr = InetSocketAddr::from_str("0.0.0.0:12345").unwrap();

        let mut buf = [0 as u8; 50];
        let res = stream.read(&mut buf);
        println!("{:?}", res);
        let session = BrontozaurSession::with(
            stream,
            local_node.private_key(),
            inet_addr,
        )
        .expect("Unable to establish session with the remote peer");
        debug!("session established!");
    });

    let str_onion_address = onion_address.to_string();
    println!("tor: {}", str_onion_address);
    debug!("this thing on?");
    let local_node = LocalNode::new(bitcoin::secp256k1::SECP256K1);
    let target_addr = TargetAddr::Domain(str_onion_address, 15787);
    println!("target addr: {:?}", target_addr);
    let socks_stream = Socks5Stream::connect("127.0.0.1:9050", target_addr).unwrap();
    let mut stream = socks_stream.into_inner();
    stream.write(&[1]).unwrap();
    println!("successfully created a tor stream, now on to the brontozaur session");
    let session = BrontozaurSession::with(stream, local_node.private_key(), inet_socket_addr).unwrap();
    debug!("session established!");
}
