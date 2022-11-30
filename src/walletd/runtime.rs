use crate::bus::{
    ctl::{CtlMsg, GetKeys, Keys, SwapKeys, Token, WrappedKeyManager},
    BusMsg, ServiceBus,
};

use crate::service::Endpoints;
use crate::walletd::NodeSecrets;
use crate::{CtlServer, Error, Service, ServiceConfig, ServiceId};

use farcaster_core::swap::btcxmr::KeyManager;
use microservices::esb::{self, Handler};

pub fn run(
    config: ServiceConfig,
    wallet_token: Token,
    node_secrets: NodeSecrets,
) -> Result<(), Error> {
    let runtime = Runtime {
        identity: ServiceId::Wallet,
        wallet_token,
        node_secrets,
    };

    Service::run(config, runtime, false)
}

pub struct Runtime {
    identity: ServiceId,
    wallet_token: Token,
    node_secrets: NodeSecrets,
}

impl CtlServer for Runtime {}

impl esb::Handler<ServiceBus> for Runtime {
    type Request = BusMsg;
    type Error = Error;

    fn identity(&self) -> ServiceId {
        self.identity.clone()
    }

    fn handle(
        &mut self,
        endpoints: &mut Endpoints,
        bus: ServiceBus,
        source: ServiceId,
        request: BusMsg,
    ) -> Result<(), Self::Error> {
        match (bus, request) {
            // Control bus for issuing control commands, only accept Ctl message
            (ServiceBus::Ctl, BusMsg::Ctl(req)) => self.handle_ctl(endpoints, source, req),
            // All other pairs are not supported
            (bus, req) => Err(Error::NotSupported(bus, req.to_string())),
        }
    }

    fn handle_err(&mut self, _: &mut Endpoints, _: esb::Error<ServiceId>) -> Result<(), Error> {
        // We do nothing and do not propagate error; it's already being reported
        // with `error!` macro by the controller. If we propagate error here
        // this will make whole daemon panic
        Ok(())
    }
}

impl Runtime {
    fn handle_ctl(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: CtlMsg,
    ) -> Result<(), Error> {
        match request {
            CtlMsg::Hello => {
                debug!("Received Hello from {}", source);
            }

            CtlMsg::CreateSwapKeys(public_offer, wallet_token) => {
                if wallet_token != self.wallet_token {
                    return Err(Error::InvalidToken);
                }
                let wallet_index = self.node_secrets.increment_wallet_counter();
                let key_manager = KeyManager::new(self.node_secrets.wallet_seed, wallet_index)?;
                let swap_keys = SwapKeys {
                    key_manager: WrappedKeyManager(key_manager),
                    public_offer,
                };
                endpoints.send_to(
                    ServiceBus::Ctl,
                    self.identity(),
                    ServiceId::Farcasterd,
                    BusMsg::Ctl(CtlMsg::SwapKeys(swap_keys)),
                )?;
            }

            CtlMsg::GetKeys(GetKeys(wallet_token)) => {
                if wallet_token != self.wallet_token {
                    return Err(Error::InvalidToken);
                }
                trace!("sent Secret request to farcasterd");
                endpoints.send_to(
                    ServiceBus::Ctl,
                    ServiceId::Wallet,
                    ServiceId::Farcasterd,
                    BusMsg::Ctl(CtlMsg::Keys(Keys(
                        self.node_secrets.peerd_secret_key,
                        self.node_secrets.node_id(),
                    ))),
                )?;
            }

            req => {
                error!(
                    "BusMsg {} is not supported by the CTL interface",
                    req.to_string()
                );
            }
        }

        Ok(())
    }
}
