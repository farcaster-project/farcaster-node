# Command flows

## Connect peer
1. Local flow
  - user->cli: `connect <peer>` command
  - cli->farcasterd: `ConnectPeer`
  - farcasterd: launches `peerd` with remote peer specified
  - peerd->farcasterd: `Hello`
	- farcasterd: registers swapd
  - peerd: establishes TCP connection with the remote peer
  - peerd: sends remote peer `Init` message
2. Remote flow
  - peerd-listener: accepts TCP connection and spawns new peerd instance
  - peerd->farcasterd: `Hello`
	- farcasterd: registers swapd
	- peerd: receives `Init` message
	- #TODO peerd->farcasterd: forwards `Init` message
	- #TODO farcasterd: verifies assets from the init message asking fungibled on provided assets via extension mechanism
3. #TODO If some of the assets or features not supported
  - farcasterd->peerd: `Terminate`
  - farcasterd: removes peerd from the list of available connections
  - peerd: terminates connection and shuts down
  - farcasterd: force killing in 1 sec

## Ping-pong
1. Local flow
  - tcp->peerd: Timeout
  - peerd: checks that the preivous `pong` was responded, otherwise marks the remote peer unresponding
  - peerd: sends remote peer `Ping` message
2. Remote flow
  - peerd: receives `Ping` message
  - peerd: prepares and sends `Pong` response
3. Local flow
  - peerd: receives `Pong` message and proceeds

## Swap creation
1. Local flow
	- user->cli: `create channel <peer>` command
	- cli->farcasterd: `OpenSwaplWith`
	- farcasterd: launches `swapd` and waits for it's connection
	- swapd->farcasterd: `Hello`
	- farcasterd: registers swapd
	- farcasterd->swapd: `OpenSwaplWith`
	- swapd->peerd: `OpenChannel` message
	- peerd: sends remote peer `OpenChannel` message
2. Remote flow
	- peerd: receives `OpenChannel` message
	- peerd->farcasterd: forwards `OpenChannel` message
	- farcasterd: launches `swapd` and waits for it's connection
	- swapd->farcasterd: `Hello`
	- farcasterd: registers swapd
	- farcasterd->swapd: `AcceptChannelFrom`
	- swapd->peerd: `AcceptChannel` message
	- peerd: sends remote peer `AcceptChannel` message
3. Local flow
	- peerd: receives `AcceptChannel` message
	- peerd->swapd: forwards `AcceptChannel` message
	- swapd: marks channel as accepted
4. #TODO Continue:
	* local->remote: FundingCreated
	* remote->local: FundingSigned
	* local<->remote: FundingLocked
