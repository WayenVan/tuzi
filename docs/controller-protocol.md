# Controller protocol

`tu dds controller` is a JSON Lines bridge between an editor plugin and the
Tuzi instances it launches.

## Start

```sh
tu dds controller
```

The first stdout line contains the controller's DDS address:

```json
{"event":"controller-ready","protocol_version":1,"peer_id":701}
```

Write one JSON object per line to stdin. Read one JSON object per line from
stdout. Keep the process running for the lifetime of the editor.

Clients must check `protocol_version` before sending requests. Version `1` is
the protocol documented on this page.

## Connect a Tuzi

Register a fresh launch token:

```json
{"request_id":1,"op":"register","token":"random-launch-token"}
```

```json
{"request_id":1,"ok":true}
```

Then launch Tuzi in the editor's terminal using the controller `peer_id`:

```sh
TUZI_DDS_PARENT=701 \
TUZI_DDS_TOKEN=random-launch-token \
tuzi /project
```

The controller reports the Tuzi peer when the handshake completes:

```json
{"event":"tuzi-ready","token":"random-launch-token","peer_id":902}
```

Generate a new unpredictable token for every launch.

## Receive events

Messages from controlled Tuzi instances are emitted as:

```json
{"event":"message","peer_id":902,"kind":"hover","body":{"Hover":{"path":"/project/src/main.rs"}}}
```

A Tuzi launched with `--runtime-config '{"config":{"dds":{"open":"parent"}}}'` sends its ordinary open actions
to the controller in the same event envelope:

```json
{"event":"message","peer_id":902,"kind":"open","body":{"Open":{"paths":["/project/src/main.rs"]}}}
```

The default abilities are `hover,cd,yank,renamed,task-done`. Override them at
startup when needed:

```sh
tu dds controller --abilities hover,cd,renamed
```

Messages from Tuzi instances that did not complete a registered-token
handshake are ignored.

## Send a message

After `tuzi-ready`, address a controlled Tuzi by its `peer_id`. Change its
directory and selection with the typed `set-state` operation:

```json
{"request_id":2,"op":"set-state","peer_id":902,"state":{"path":"/project","selection":["README.md"]}}
```

```json
{"request_id":2,"ok":true,"status":"queued"}
```

For `set-state` and `publish`, `status: "queued"` means the target was
controlled, online, declared the requested ability, and the message was queued
for DDS delivery. It does not claim that Tuzi has already applied the action.
An error response has `ok: false` and an `error` string.

`publish` is the escape hatch for custom, non-reserved kinds:

```json
{"request_id":3,"op":"publish","peer_id":902,"kind":"plugin-event","data":{"value":1}}
```

The controller rejects a `peer_id` that did not complete its registered-token
handshake, is currently offline, or did not declare the requested ability.

## Inspect and manage clients

```json
{"request_id":4,"op":"list"}
```

```json
{"request_id":4,"ok":true,"clients":[{"token":"random-launch-token","peer_id":902,"online":true}]}
```

Cancel a token that has not completed its handshake:

```json
{"request_id":5,"op":"cancel-register","token":"random-launch-token"}
```

Stop controlling one Tuzi without terminating it:

```json
{"request_id":6,"op":"detach","peer_id":902}
```

Check the controller process and protocol version:

```json
{"request_id":7,"op":"ping"}
```

```json
{"request_id":7,"ok":true,"protocol_version":1,"peer_id":701}
```

## Disconnect

When a controlled Tuzi disconnects and does not reconnect within the short
failover grace period, the controller removes it and emits:

```json
{"event":"tuzi-left","token":"random-launch-token","peer_id":902}
```

Closing controller stdin shuts the controller down.

## Request IDs

`request_id` matches a response to a request. It only needs to be unique among
currently pending requests. Responses can arrive asynchronously, so callers
should keep a `request_id -> callback` map and apply a timeout to each request.

## Identity model

The launch token authorizes only the initial handshake. Runtime operations use
one `peer_id` at a time. Tuzi accepts `set-state` from its saved parent peer
only, and the DDS server binds every sender ID to the connection that completed
the `Hi` handshake so another client cannot spoof the parent ID.
