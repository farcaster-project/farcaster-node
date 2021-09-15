// LNP Node: node running lightning network protocol and generalized lightning
// channels.
// Written in 2020 by
//     Dr. Maxim Orlovsky <orlovsky@pandoracore.com>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the MIT License
// along with this software.
// If not, see <https://opensource.org/licenses/MIT>.

pub mod bitcoin_syncer;
pub mod monero_syncer;
pub mod syncer_state;
pub mod types;

#[cfg(feature = "shell")]
pub mod opts;
pub mod runtime;

#[cfg(feature = "shell")]
pub use opts::Opts;
pub use runtime::run;
pub use runtime::SyncerServers;
pub use types::*;
