# Controller protocol

`tu dds controller` is a JSON Lines bridge between an editor plugin and the
Tuzi instances it launches.

## Start

```sh
tu dds controller
```

The first stdout line contains the controller's DDS address:

```json
{"event":"controller-ready","protocol_version":2,"peer_id":701}
```

Write one JSON object per line to stdin. Read one JSON object per line from
stdout. Keep the process running for the lifetime of the editor.

Clients must check `protocol_version` before sending requests. Version `2` is
the protocol documented on this page.

Implemented operations are `register`, `cancel-register`, `list`, `detach`,
`update-tab`, `get-tabs`, `switch-tab`, `get-state`, `reveal`, `restore-state`,
`publish`, and `ping`.

## Command reference

| `op` | Target | Required fields | Success result |
|---|---|---|---|
| `register` | handshake | `token` | registration accepted |
| `cancel-register` | handshake | `token` | pending registration removed |
| `list` | controller | none | `clients` array |
| `detach` | controlled Tuzi | `peer_id` | control relationship removed |
| `update-tab` | controlled Tuzi | `peer_id`, `update` | `status: "queued"` |
| `get-tabs` | controlled Tuzi | `peer_id` | ordered `tabs` and `active_tab_id` |
| `switch-tab` | controlled Tuzi | `peer_id`, `tab_id` | `status: "queued"` |
| `get-state` | controlled Tuzi | `peer_id` | complete `state` snapshot |
| `reveal` | controlled Tuzi | `peer_id`, absolute `path` | `status: "queued"` |
| `restore-state` | controlled Tuzi | `peer_id`, `state` | `status: "queued"` |
| `publish` | controlled Tuzi | `peer_id`, `kind`, optional `data` | `status: "queued"` |
| `ping` | controller | none | protocol version and controller peer ID |

Every request requires an integer `request_id`. `register` and
`cancel-register` use the launch token; runtime operations use one `peer_id`
at a time. There is no controller broadcast operation.

Request shapes:

```json
{"request_id":1,"op":"register","token":"random-launch-token"}
{"request_id":2,"op":"cancel-register","token":"random-launch-token"}
{"request_id":3,"op":"list"}
{"request_id":4,"op":"detach","peer_id":902}
{"request_id":5,"op":"update-tab","peer_id":902,"update":{"path":"/project","selection":["README.md"]}}
{"request_id":11,"op":"get-tabs","peer_id":902}
{"request_id":12,"op":"switch-tab","peer_id":902,"tab_id":3}
{"request_id":6,"op":"get-state","peer_id":902}
{"request_id":7,"op":"reveal","peer_id":902,"path":"/project/src/main.rs"}
{"request_id":8,"op":"restore-state","peer_id":902,"state":{"version":1,"active_tab":0,"tabs":[{"cwd":"/project","cursor":"/project/README.md","selection":[],"expanded":["/project/src"]}]}}
{"request_id":9,"op":"publish","peer_id":902,"kind":"plugin-event","data":{"value":1}}
{"request_id":10,"op":"ping"}
```

## Event reference

Controller responses contain `request_id`. Unsolicited lifecycle and DDS
messages contain `event` instead:

| `event` | Fields | Meaning |
|---|---|---|
| `controller-ready` | `protocol_version`, `peer_id` | controller started |
| `tuzi-ready` | `token`, `peer_id` | registered launch completed |
| `message` | `peer_id`, `kind`, `body` | message from a controlled Tuzi |
| `tuzi-exit` | `peer_id`, `state` | final restorable session snapshot on graceful exit |
| `tuzi-left` | `token`, `peer_id` | controlled Tuzi left after grace period |

Malformed input that cannot be decoded far enough to recover a request ID is
reported as `{ "ok": false, "error": "..." }` without `request_id`.

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

The token is used only to authorize this handshake. After `tuzi-ready`, use
the returned `peer_id` for runtime operations.

## Receive events

Messages from controlled Tuzi instances are emitted as (when the controller
subscribes to that event kind):

```json
{"event":"message","peer_id":902,"kind":"hover","body":{"Hover":{"path":"/project/src/main.rs"}}}
```

A Tuzi launched with `--runtime-config '{"config":{"dds":{"open":"parent"}}}'` sends its ordinary open actions
to the controller in the same event envelope:

```json
{"event":"message","peer_id":902,"kind":"open","body":{"Open":{"paths":["/project/src/main.rs"]}}}
```

The default abilities are `cd,yank,renamed,task-done`. Cursor movement can
produce frequent `hover` events, so subscribe explicitly when needed:

```sh
tu dds controller --abilities hover,cd,renamed
```

A controlled Tuzi sends these implicit state events directly to this parent
when it is online and advertised the matching ability. An event explicitly
listed in Tuzi's `dds.broadcast` setting is public instead; the controller
receives that broadcast once if it subscribed. Without a controlled parent,
implicit events remain local unless publicly broadcast.

Messages from Tuzi instances that did not complete a registered-token
handshake are ignored.

## Send a message

After `tuzi-ready`, address a controlled Tuzi by its `peer_id`. Change its
directory and selection with the typed `update-tab` operation:

```json
{"request_id":2,"op":"update-tab","peer_id":902,"update":{"path":"/project","selection":["README.md"]}}
```

```json
{"request_id":2,"ok":true,"status":"queued"}
```

For `update-tab`, `switch-tab`, `reveal`, `restore-state`, and `publish`, `status: "queued"` means the
target was controlled, online, declared the requested ability, and the message
was queued for DDS delivery. It does not claim that Tuzi has already applied
the action. An error response has `ok: false` and an `error` string.

`reveal` tries to show an absolute path in the active tab without changing
that tab's root or selection. Tuzi expands the path's ancestors and moves the
cursor to it if it becomes visible. A missing path or a path outside the
active tab's root produces a local error toast. A path that disappears during
asynchronous listing produces a local information toast. The controller
receives no completion event.

Both fields in `update` are optional. `path` changes the active tab's root.
`selection` replaces its selection and defaults to an empty list, so omitting
it clears the current selection. Relative selection paths are resolved against
the tab root; missing paths and paths outside that root are ignored.

Use `get-tabs` to read live tab IDs and their order without building a
restorable session snapshot:

```json
{"request_id":11,"op":"get-tabs","peer_id":902}
{"request_id":11,"ok":true,"active_tab_id":3,"tabs":[{"id":0,"cwd":"/project"},{"id":3,"cwd":"/other"}]}
```

Use one of those IDs to switch the active tab:

```json
{"request_id":12,"op":"switch-tab","peer_id":902,"tab_id":0}
```

The controller acknowledges that the request was queued. If the tab has
disappeared before Tuzi applies it, Tuzi shows a local warning. IDs identify
tabs only within the current Tuzi process. `get-state` and `restore-state`
continue to use an `active_tab` array index, and restoring a session may
allocate new runtime IDs.

Use `get-state` to request the currently visible complete session:

```json
{"request_id":10,"op":"get-state","peer_id":902}
```

The controller waits for a reply from that Tuzi and returns one final response
with the same `request_id`:

```json
{"request_id":10,"ok":true,"state":{"version":1,"active_tab":0,"tabs":[{"cwd":"/project","cursor":"/project/README.md","selection":[],"expanded":["/project/src"]}]}}
```

The snapshot uses the same schema as `restore-state`, including all tabs and
their roots, cursors, selections, and expanded directories. It excludes a
replacement session still being restored off-screen. Tuzi validates the
snapshot before replying so it can be submitted to `restore-state` while its
paths still exist. If a path has vanished or a restore limit is exceeded,
`get-state` returns an error instead of a partial snapshot. The controller
also returns an error if the peer leaves, is detached, or does not reply
within five seconds. `get-tabs` has the same request timeout. Unlike
state-changing operations, these queries have no
intermediate `queued` response.

Use `restore-state` to replace the whole session:

```json
{
  "request_id":3,
  "op":"restore-state",
  "peer_id":902,
  "state":{
    "version":1,
    "active_tab":1,
    "tabs":[
      {
        "cwd":"/project",
        "cursor":"/project/src/main.rs",
        "selection":[],
        "expanded":["/project/src"]
      },
      {
        "cwd":"/project/tests",
        "cursor":null,
        "selection":["/project/tests/input.txt"],
        "expanded":[]
      }
    ]
  }
}
```

All snapshot paths are absolute. `version` must be `1`, `tabs` must contain
1–32 entries, and `active_tab` is a zero-based array index. Each cwd must be
an existing directory; cursor, selection, and expanded paths must exist inside
that cwd, and expanded paths must be directories. Tuzi automatically adds the
cursor's directory ancestors to `expanded`; selection does not expand its
ancestors.

The limits are 256 explicit expanded paths per tab, 4096 selection paths per
request, and a maximum relative path depth of 64. Tuzi builds every new tab
off-screen and commits them together. Any validation or listing failure keeps
the prior session unchanged and produces a local warning. `queued` remains a
delivery acknowledgement, not a restore-success acknowledgement.

`publish` is the escape hatch for custom, non-reserved kinds:

```json
{"request_id":3,"op":"publish","peer_id":902,"kind":"plugin-event","data":{"value":1}}
```

The controller rejects a `peer_id` that did not complete its registered-token
handshake, is currently offline, or did not declare the requested ability.
Consequently, `publish` is usable only when the target peer advertised its
custom `kind` as an ability.

## Inspect and manage clients

```json
{"request_id":4,"op":"list"}
```

```json
{"request_id":4,"ok":true,"clients":[{"token":"random-launch-token","peer_id":902,"online":true}]}
```

A missing peer remains listed with `online:false` during the 500ms failover
grace period. If it does not reconnect, it is removed and `tuzi-left` is
emitted.

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
{"request_id":7,"ok":true,"protocol_version":2,"peer_id":701}
```

## Disconnect

On a normal exit, a controlled Tuzi sends its final session snapshot directly
to the controller. The controller emits this event before `tuzi-left`:

```json
{"event":"tuzi-exit","peer_id":902,"state":{"version":1,"active_tab":0,"tabs":[{"cwd":"/project","cursor":null,"selection":[],"expanded":[]}]}}
```

`state` uses the same schema as `get-state` and `restore-state`. This is a
best-effort notification: a forced exit, disconnected parent, or invalid
snapshot may leave only the later `tuzi-left` event.

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
While a `get-state` or `get-tabs` is pending, the controller rejects any second request with
the same `request_id` without attaching that ID to the rejection, so it cannot
be mistaken for the first request's final response.

## Identity model

The launch token authorizes only the initial handshake. Runtime operations use
one `peer_id` at a time. Tuzi accepts `update-tab`, `switch-tab`, `get-tabs`, `get-state`, `reveal`, and `restore-state` from its
saved parent peer only, and the DDS server binds every sender ID to the
connection that completed the `Join` handshake so another client cannot spoof
the parent ID. Tuzi also checks that a directed control operation is supported
and that its JSON schema is valid; rejected messages produce a local warning.
