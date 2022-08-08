use crate::{
    automata::Event,
    rpc::{Failure, FailureCode, Request},
    swapd::runtime::Runtime,
    CtlServer, Endpoints, Error, LogStyle, ServiceId,
};
use microservices::esb::{self, Handler};

use super::get_swap_id;

impl Runtime {
    /// Processes incoming RPC or peer requests updating state - and switching to a new state, if
    /// necessary. Returns bool indicating whether a successful state update happened
    pub fn process(
        &mut self,
        endpoints: &mut Endpoints,
        source: ServiceId,
        request: Request,
    ) -> Result<bool, Error> {
        let swapd_service_id = self.identity();
        let swap_id = get_swap_id(&swapd_service_id);
        let event = Event::with(endpoints, swapd_service_id, source.clone(), request);
        let updated_state = match self.process_event(event) {
            Ok(_) => {
                // Ignoring possible reporting errors here and after: do not want to
                // halt the channel just because the client disconnected
                let _ = self.report_progress_message_to(
                    endpoints, source, "", // self.state.state_machine.info_message(swap_id),
                );
                true
            }
            // We pass ESB errors forward such that they can fail the channel.
            // In the future they can be caught here and used to re-iterate sending of the same
            // message later without channel halting.
            Err(err @ Error::Esb(_)) => {
                error!(
                    "{} due to ESB failure: {}",
                    "Failing channel".err(),
                    err.err_details()
                );
                self.report_failure_to(
                    endpoints,
                    self.enquirer(),
                    Failure {
                        code: FailureCode::Unknown, // FIXME
                        info: err.to_string(),
                    },
                );
                return Err(err);
            }
            Err(other_err) => {
                error!("{}: {}", "Swap error".err(), other_err.err_details());
                self.report_failure_to(
                    endpoints,
                    self.enquirer(),
                    Failure {
                        code: FailureCode::Unknown,
                        info: other_err.to_string(),
                    },
                );
                false
            }
        };
        // if updated_state {
        //     self.save_state()?;
        //     info!(
        //         "ChannelStateMachine {} switched to {} state",
        //         self.state.channel.active_channel_id(),
        //         self.state.state_machine
        //     );
        // }
        Ok(updated_state)
    }

    fn process_event(&mut self, event: Event<Request>) -> Result<(), Error> {
        Ok(())
    }
}
