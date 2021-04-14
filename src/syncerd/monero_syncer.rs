#![allow(dead_code, unused_must_use)]

use std::io;

use async_trait::async_trait;

use farcaster_core::tasks::*;
use farcaster_core::events::*;
use farcaster_core::interactions::syncer::Syncer;

struct MoneroSyncer {}
#[async_trait]
impl Syncer for MoneroSyncer {
    async fn abort(&mut self, task: Abort) {}
    async fn watch_height(&mut self, task: WatchHeight) {}
    async fn watch_address(&mut self, task: WatchAddress) -> io::Result<()> {todo!()}
    async fn watch_transaction(&mut self, task: WatchTransaction) -> io::Result<()> {todo!()}
    async fn broadcast_transaction(&mut self, task: BroadcastTransaction) -> io::Result<()> {todo!()}
    async fn poll(&mut self) -> io::Result<Vec<Event>> {todo!()}
}
