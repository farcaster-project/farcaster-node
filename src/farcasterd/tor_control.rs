use std::net::SocketAddr;

use internet2::addr::InetSocketAddr;
use tokio::net::TcpStream;
use torut::{
    control::{ConnError, UnauthenticatedConn},
    onion::OnionAddressV3,
};

use crate::bus::HiddenServiceInfo;

pub fn create_v3_onion_service(
    bind_addr: InetSocketAddr,
    public_port: u16,
    control_addr: InetSocketAddr,
    existing_hidden_services: &Vec<HiddenServiceInfo>,
) -> Result<OnionAddressV3, ConnError> {
    use tokio::runtime::Builder;
    let rt = Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        // Connect to the Tor control socket
        let s = TcpStream::connect(control_addr.to_string()).await.unwrap();
        let mut uc = UnauthenticatedConn::new(s);

        // Authenticate to the tor socket
        let proto_info = uc.load_protocol_info().await.unwrap();
        if let Some(tor_auth_data) = proto_info.make_auth_data()? {
            uc.authenticate(&tor_auth_data).await?;
        }
        let mut ac = uc.into_authenticated().await;

        // We have to set a dummy event handler, even if we are not reading any
        // events
        ac.set_async_event_handler(Some(|_| async move { Ok(()) }));

        // Take ownership (meaning control) the Tor instance
        ac.take_ownership().await.unwrap();

        // Check if we previously created this hidden service and used it on the same bind
        if !existing_hidden_services.is_empty() {
            let res = ac.get_info("onions/detached").await.unwrap();
            for existing_hidden_service in existing_hidden_services.iter() {
                for line in res.lines() {
                    if line.contains(
                        &existing_hidden_service
                            .onion_address
                            .0
                            .get_address_without_dot_onion(),
                    ) && existing_hidden_service.bind_address == bind_addr
                    {
                        return Ok(existing_hidden_service.onion_address.0);
                    }
                }
            }
        }

        let bind_socket_addr = match bind_addr {
            InetSocketAddr::IPv4(socket) => SocketAddr::V4(socket),
            InetSocketAddr::IPv6(socket) => SocketAddr::V6(socket),
            _ => {
                return Err(ConnError::InvalidFormat);
            }
        };

        // Generate a secret service
        let key = torut::onion::TorSecretKeyV3::generate();
        ac.add_onion_v3(
            &key,
            true, // Detach to ensure the hidden service remains on the tor instance after we disconnect from the control socket
            false,
            false,
            None,
            &mut [(public_port, bind_socket_addr)].iter(),
        )
        .await?;

        // Rescind ownership of the control port
        ac.drop_ownership().await?;

        println!("public key: {}", key.public().get_onion_address());
        Ok(key.public().get_onion_address())
    })
}
