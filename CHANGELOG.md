# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/farcaster-project/farcaster-node/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/farcaster-project/farcaster-node/compare/6c42e2892c18d600d4597356a9f827d58026b54a...v0.1.0
