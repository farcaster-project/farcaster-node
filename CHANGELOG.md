# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- fix typos by @Lederstrumpf in https://github.com/farcaster-project/farcaster-node/pull/381
- Reduce log level of Docker Compose and add container name declarations for each container by @sethforprivacy in https://github.com/farcaster-project/farcaster-node/pull/386
- Taker Command: Display exchange rate when reviewing offer by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/394
- Bitcoin syncer: Unblock event loop by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/388
- Peerd: Handle remote shutdown by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/391
- Peerd single port multi connections by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/393
- Relax existing listening address warning to debug level by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/398
- farcasterd: only accepts TakerCommit from peerd by @zkao in https://github.com/farcaster-project/farcaster-node/pull/314
- Fee estimation by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/395
- Lws support by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/401
- Lws logging by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/413
- Config: Add lws to example config file by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/415
- Merge syncer address tests by @Lederstrumpf in https://github.com/farcaster-project/farcaster-node/pull/416
- Close and delete wallet after sweep by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/418
- Monero Syncer: Require a minimum sweep balance by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/417
- Peer info: Use remote address to assemble internal id by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/420
- Farcasterd: Simplify LaunchSwap peer ServiceId recreation by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/421
- add userpass auth option for bitcoin rpc by @Lederstrumpf in https://github.com/farcaster-project/farcaster-node/pull/439
- State clean up by @zkao in https://github.com/farcaster-project/farcaster-node/pull/442
- Align dependencies with lnp-node e81e693d1a92fc5aef10c423648db43e4b755e97 by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/404
- Cli: Fix make command failure reporting by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/436
- Make offer / take offer cli yaml serializable by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/437
- GRPCD: A service daemon for a farcaster grpc api by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/443
- Reconnect peerd if connection is dropped during running swap by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/426
- Peerd: Remove UpdateSwapId request handling by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/456
- Functionality: Add ability to revoke offers by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/463
- Make progress serializable - alternative by @h4sh3d in https://github.com/farcaster-project/farcaster-node/pull/479
- Peerd: Only send whitelisted Msg's over the bridge by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/475
- feature: make progress waiting on new messages by @h4sh3d in https://github.com/farcaster-project/farcaster-node/pull/482
- Farcasterd: Check if peerd listen launched successfully by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/484
- Swapd runtime refactor: extract self contained data structures into their own files -- syncer client extraction by @zkao in https://github.com/farcaster-project/farcaster-node/pull/445
- Syncer: Only farcaster is allowed to terminate by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/494
- Grpc: Correct bridge address s/syncer/grpc/ by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/495
- extract from Swapd runtime temporal safety by @zkao in https://github.com/farcaster-project/farcaster-node/pull/447
- extract from swapd runtime State by @zkao in https://github.com/farcaster-project/farcaster-node/pull/446
- State recovery by @Lederstrumpf in https://github.com/farcaster-project/farcaster-node/pull/422
- Cli: Add endpoint to print information about a passed-in offer by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/496
- State Recovery management implementation by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/477
- Syncer: Sweep Btc by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/499
- State Recovery Swapd Implementation by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/485
- Persist funding address secret key by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/506
- Cancel swap by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/504
- Persist offer history by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/507
- Offer history by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/512
- Swap: Allow cancel for alice refundsig before arb lock by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/518
- Cli: Implement sweep address call by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/517
- Swap: Ensure a checkpoint is only set once and in the correct order by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/520
- Fix Checkpoint race condition by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/521
- Swap: Add public offer to GetInfo by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/524
- Syncer: Cache estimate fee values by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/536
- Swap: Handle monero under- and overfunding by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/534
- Swap: Handle bitcoin under- and overfunding by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/539
- Swap: Remove extra 0.02 XMR funding amount by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/544
- Bug(Wallet): persist funding address secret key for Bob Taker by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/545
- Persist monero addresses by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/540
- swapd: fee estimate from syncer (electrum) by @zkao in https://github.com/farcaster-project/farcaster-node/pull/533
- syncer trivial: replace match by map by @zkao in https://github.com/farcaster-project/farcaster-node/pull/557
- swapd: removing nesting by @zkao in https://github.com/farcaster-project/farcaster-node/pull/558
- do not use ping from ligthing peer message by @zkao in https://github.com/farcaster-project/farcaster-node/pull/556
- Syncer: Add explicit retry flag to the sweep tasks by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/548
- farcasterd: launch syncers before launching swapd, not after by @zkao in https://github.com/farcaster-project/farcaster-node/pull/555
- Swapd: create better API for handling PendingRequests by @zkao in https://github.com/farcaster-project/farcaster-node/pull/561
- farcaster-swapd-syncer race-free initiation interplay: improve sequence of msgs by @zkao in https://github.com/farcaster-project/farcaster-node/pull/574
- Manual monero sweep by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/542
- Configuration: Change Monero stagenet node by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/579
- Syncer: Rename EstimateFee to WatchEstimateFee by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/580
- Swap: Only report abort back to client by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/582
- Database: Expand to full path for lmdb file location by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/585
- Database: Log debug, not error, when removed entry does not exist by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/592
- Use `RUST_LOG` to configure loggers instead of `--verbose` flag by @h4sh3d in https://github.com/farcaster-project/farcaster-node/pull/590
- use syncer types and associated fns to avoid race condition between swapd, monero syncer and bitcoin syncer by @zkao in https://github.com/farcaster-project/farcaster-node/pull/588
- Update core to version 0.5.0 by @h4sh3d in https://github.com/farcaster-project/farcaster-node/pull/567
- Checkpointing: Remove multipart message chunking by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/595
- Monero strict encoding types by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/599
- Types: Use bitcoin secret key wherever possible by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/600
- Refactor: Sweep monero rename by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/601
- Refactor: Use core's Blockchain instead of syncer's Coin type by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/602
- Peer: Use Brontozaur for session encryption by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/605
- Types: Use core's network conversion by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/603
- Grpc: Fix test and binding address by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/606
- Farcasterd: Improve report by @TheCharlatan in https://github.com/farcaster-project/farcaster-node/pull/610

## [0.2.0] - 2021-12-15

Internal stress-testing bugfixes. Preparation for external release.

- Support running parallel swaps over different peer connections through different
  ports

## [0.1.0] - 2021-12-10

Initial version of Farcaster Node :tada:

### Added

- Bitcoin SegWit v0 support as arbitrating chain
- Monero support as accordant chain
- Bitcoin and Monero syncers
- Swap daemon to manage single swap execution
- Peer daemon to manage p2p connection during swaps
- Farcaster daemon to orchestrate the micro-services
- Swap cli to control farcasterd and other services

[Unreleased]: https://github.com/farcaster-project/farcaster-node/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/farcaster-project/farcaster-node/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/farcaster-project/farcaster-node/compare/6c42e2892c18d600d4597356a9f827d58026b54a...v0.1.0
