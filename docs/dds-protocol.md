# DDS message reference

DDS is Tuzi's local Unix-socket message bus. Every wire message is one JSON
line with this envelope:

```json
{"receiver":0,"sender":701,"body":{"Hover":{"path":"/project/README.md"}}}
```

`receiver: 0` means broadcast. A non-zero receiver is one peer ID. The server
binds `sender` to the connection that completed the `Join` handshake and rejects
messages that claim another sender ID.

## Supported kinds

| Kind | Direction | Route | Ability | Purpose |
|---|---|---|---|---|
| `join` | client → server | handshake | no | declare peer ID and abilities |
| `sync` | server → all clients | broadcast | no | publish the current peer table |
| `attach` | Tuzi → parent | direct | no | complete a token-authorized launch |
| `open` | Tuzi → parent | direct | no | ask the host to open paths |
| `update-tab` | parent → Tuzi | direct | `update-tab` | update the active tab |
| `get-tabs` | parent → Tuzi | direct | `get-tabs` | request live tab IDs, order, and roots |
| `tabs` | Tuzi → parent | direct | no | reply to one `get-tabs` request |
| `switch-tab` | parent → Tuzi | direct | `switch-tab` | select a live tab by ID |
| `get-state` | parent → Tuzi | direct | `get-state` | request the visible session snapshot |
| `state` / `state-error` | Tuzi → parent | direct | no | reply to one `get-state` request |
| `session-end` | Tuzi → parent | direct | no | send a restorable snapshot during graceful exit |
| `reveal` | parent → Tuzi | direct | `reveal` | reveal a path in the active tab |
| `set-home` | parent → Tuzi | direct | `set-home` | change the session home used by `cd @home` |
| `restore-state` | parent → Tuzi | direct | `restore-state` | replace the complete session atomically |
| `cd` | Tuzi → parent/subscribers | parent direct or public broadcast | `cd` | active tab root changed |
| `hover` | Tuzi → parent/subscribers | parent direct or public broadcast | `hover` | cursor path changed |
| `yank` | Tuzi → parent/subscribers | parent direct or public broadcast | `yank` | copy/cut state changed |
| `renamed` | Tuzi → parent/subscribers | parent direct or public broadcast | `renamed` | a path was renamed |
| `task-done` | Tuzi → parent/subscribers | parent direct or public broadcast | `task-done` | file task completed |
| custom | any peer → subscribers/peer | either | custom kind | extension message |

Broadcast delivery requires the receiver to declare the matching ability or
the wildcard ability `*`. Direct DDS routing itself does not inspect abilities;
controller performs an ability check before sending, and controlled Tuzi does
the final receiver-side check. For implicit state events, Tuzi also checks its
parent's advertised abilities before sending directly. `tu dds controller`
advertises `cd,yank,renamed,task-done` by default; use `--abilities` to replace
that list and include `hover` when cursor events are needed. Direct protocol
messages such as `Attach`, `Open`, `State`, `Tabs`, and `SessionEnd` do not
require a matching receiver ability.

## Handshake messages

```json
{"Join":{"abilities":["get-state","get-tabs","restore-state","reveal","set-home","switch-tab","update-tab"]}}
```

`Join` must be the first message on a connection. A second `Join`, a duplicate peer
ID, or a later message with a different sender ID closes that connection.

```json
{"Sync":{"peers":[{"id":701,"abilities":["cd","yank","renamed","task-done"]},{"id":902,"abilities":["get-state","get-tabs","restore-state","reveal","set-home","switch-tab","update-tab"]}]}}
```

`Sync` is emitted whenever the connected peer table changes.

```json
{"Attach":{"token":"random-launch-token"}}
```

`Attach` is sent directly to the configured parent. The enclosing sender is the
new Tuzi peer ID. The controller accepts it only when the token was registered.

## Control messages

```json
{"Open":{"paths":["/project/src/main.rs"]}}
```

`open` is always direct to the parent. It is emitted for ordinary opens when
`dds.open` selects the parent route; interactive opens remain local.

`update-tab` is encoded as a custom body on the wire, but is a reserved
controller operation rather than a general `publish` kind:

```json
{"Custom":{"kind":"update-tab","data":{"path":"/project","selection":["README.md"]}}}
```

`path` and `selection` are optional. `selection` replaces the current selection
and defaults to an empty list. A controlled Tuzi accepts this operation only
from its saved parent and validates the JSON schema before delivery.

`switch-tab` is also a reserved custom body. It targets a runtime tab ID:

```json
{"Custom":{"kind":"switch-tab","data":{"tab_id":3}}}
```

If that tab has closed before Tuzi handles the message, Tuzi reports the
failure locally. The controller does not receive an execution result.

`reveal` is another reserved custom body. Its `path` must be absolute:

```json
{"Custom":{"kind":"reveal","data":{"path":"/project/src/main.rs"}}}
```

Tuzi tries to expand the active tab's tree and place the cursor on the path.
Failures appear only in the Tuzi UI; there is no execution acknowledgement.

`set-home` is another reserved custom body. It changes the session home, the
single global directory `cd @home` (`g=`) goes to in every tab. `path` must be
absolute:

```json
{"Custom":{"kind":"set-home","data":{"path":"/project"}}}
```

It changes only the home. No tab moves. If the path is missing or is not a
directory, Tuzi keeps the previous home and shows a local warning; there is no
execution acknowledgement.

`restore-state` is also encoded as a reserved custom body:

```json
{"Custom":{"kind":"restore-state","data":{"version":1,"active_tab":0,"tabs":[{"cwd":"/project","cursor":"/project/README.md","selection":[],"expanded":["/project/src"]}]}}}
```

The snapshot is the sole source of truth for tab count, order, active tab,
roots, cursors, selections, and expanded directories. It may also carry a
top-level `home` (an absolute existing directory, shared by all tabs and never
stored inside `tabs`); when present it replaces the session home in the same
commit as the tabs, and when absent the current home is kept. Tuzi fully validates it,
builds the replacement trees off-screen, and atomically swaps sessions only
after every lazy listing succeeds. Failure leaves the visible session intact.

`get-state` and its replies use typed direct bodies. The controller assigns
an internal `query_id`, correlates the reply with the original JSON Lines
`request_id`, and accepts it only from the requested controlled peer:

```json
{"GetState":{"query_id":1}}
{"State":{"query_id":1,"state":{"version":1,"active_tab":0,"tabs":[{"cwd":"/project","cursor":null,"selection":[],"expanded":[]}]}}}
{"StateError":{"query_id":1,"error":"invalid tab 0: ..."}}
```

`get-tabs` uses the same query ID correlation but returns only live tab
metadata. Array order is the current visual tab order:

```json
{"GetTabs":{"query_id":2}}
{"Tabs":{"query_id":2,"active_tab_id":3,"tabs":[{"id":0,"cwd":"/project"},{"id":3,"cwd":"/other"}]}}
```

Runtime tab IDs are valid only for this Tuzi process. `SessionState.active_tab`
remains an array index so saved sessions do not depend on runtime IDs.

On graceful exit, a controlled Tuzi sends the same snapshot schema to its
parent without a preceding request:

```json
{"SessionEnd":{"state":{"version":1,"active_tab":0,"tabs":[{"cwd":"/project","cursor":null,"selection":[],"expanded":[]}]}}}
```

This is a best-effort notification. Forced termination, a disconnected parent,
or a snapshot that cannot be validated may prevent it from being sent.

## State events

```json
{"Cd":{"path":"/project"}}
{"Hover":{"path":"/project/src/main.rs"}}
{"Hover":{"path":null}}
{"Yank":{"paths":["/project/README.md"],"cut":false}}
{"Renamed":{"from":"/project/old","to":"/project/new"}}
{"TaskDone":{"kind":"Copy","ok":true}}
```

`TaskDone.kind` is one of `Copy`, `Move`, `Trash`, or `Delete`.

These five implicit events always reach local in-process subscribers. A
controlled Tuzi sends an event directly to its online parent when that parent
advertised the matching ability (or `*`). An event listed in
`config.dds.broadcast` is publicly broadcast instead, so every interested
DDS peer can receive it, including the parent. Tuzi chooses one external
route per event and does not send a second direct copy. Without a parent or
an explicit broadcast rule, the event stays local.

## Custom messages

```json
{"Custom":{"kind":"plugin-event","data":{"value":1}}}
```

Custom `data` may be any JSON value. Explicit `emit`/`pub` commands bypass the
implicit broadcast allowlist but still require DDS to be enabled.

Inside Tuzi, `emit KIND [JSON]` (usable from a keymap `run` or the `:` prompt)
broadcasts publicly, so a receiver must declare `KIND` or `*` as an ability.
`emit --parent KIND [JSON]` instead sends one direct message to the
controlling parent, which needs no ability. It never falls back to a
broadcast: without a parent it shows `emit --parent requires a controlling
parent`, and with an offline parent it shows `controller unavailable`. Both
forms also reach local subscribers, and neither accepts a reserved kind or a
kind starting with `-`. The built-in
names `join`, `sync`, `attach`, `open`, `cd`, `hover`, `yank`, `renamed`, and
`task-done`, plus `get-state`, `get-tabs`, `state`, `state-error`, `tabs`, and
`session-end` are reserved. Controller additionally reserves `update-tab`,
`switch-tab`, `restore-state`, `reveal`, and `set-home` for their controller operations.

## Privacy and authentication

- Socket and runtime directory access are restricted to the current OS user.
- The server binds sender identity to one established connection.
- The launch token authorizes only `Attach`; it is not a runtime address.
- Runtime control uses one peer ID at a time.
- Controlled Tuzi verifies that directed control came from its saved parent
  and that it declared the requested state-operation ability.
- `queued` means best-effort submission, not execution acknowledgement.
