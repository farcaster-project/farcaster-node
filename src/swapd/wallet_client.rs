use crate::{service::LogStyle, ServiceId};
use farcaster_core::{swap::SwapId, transaction::TxLabel};

pub fn log_tx_received(swap_id: SwapId, txlabel: TxLabel) {
    info!(
        "{} | {} transaction received from {}",
        swap_id.bright_blue_italic(),
        txlabel.bright_white_bold(),
        ServiceId::Wallet
    );
}
