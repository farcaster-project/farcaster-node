#![allow(dead_code, unused_must_use)]

use async_trait::async_trait;

use farcaster_core::tasks::*;
use farcaster_core::interactions::syncer::Syncer;

struct MoneroSyncer {}
#[async_trait]
impl Syncer for MoneroSyncer {
    async fn abort(&mut self, task: Abort) {}
    async fn watch_height(&mut self, task: WatchHeight) {}
    async fn watch_address(&mut self, task: WatchAddress) {}
    async fn watch_transaction(&mut self, task: WatchTransaction) {}
    async fn broadcast_transaction(&mut self, task: BroadcastTransaction) {}
}
