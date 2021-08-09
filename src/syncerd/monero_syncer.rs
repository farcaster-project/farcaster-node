#![allow(dead_code, unused_must_use)]

use farcaster_core::syncer::Error;

use farcaster_core::syncer::Syncer;
use farcaster_core::syncer::*;

pub struct MoneroSyncer {}
impl Syncer for MoneroSyncer {
    fn abort(&mut self, task: Abort) -> Result<(), Error> {
        todo!()
    }
    fn watch_height(&mut self, task: WatchHeight) -> Result<(), Error> {
        todo!()
    }
    fn watch_address(&mut self, task: WatchAddress) -> Result<(), Error> {
        todo!()
    }
    fn watch_transaction(&mut self, task: WatchTransaction) -> Result<(), Error> {
        todo!()
    }
    fn broadcast_transaction(&mut self, task: BroadcastTransaction) -> Result<(), Error> {
        todo!()
    }
}
