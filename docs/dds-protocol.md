# DDS message reference

DDS is Tuzi's local Unix-socket message bus. Every wire message is one JSON
line with this envelope:

```json
{"receiver":0,"sender":701,"body":{"Hover":{"path":"/project/README.md"}}}
```

`receiver: 0` means broadcast. A non-zero receiver is one peer ID. The server
binds `sender` to the connection that completed the `Hi` handshake and rejects
messages that claim another sender ID.

## Supported kinds

| Kind | Direction | Route | Ability | Purpose |
|---|---|---|---|---|
| `hi` | client → server | handshake | no | declare peer ID and abilities |
| `hey` | server → all clients | broadcast | no | publish the current peer table |
| `ready` | Tuzi → parent | direct | no | complete a token-authorized launch |
| `open` | Tuzi → parent | direct | no | ask the host to open paths |
| `set-state` | parent → Tuzi | direct | `set-state` | update the active tab |
| `restore-state` | parent → Tuzi | direct | `restore-state` | replace the complete session atomically |
| `cd` | Tuzi → subscribers | broadcast | `cd` | active tab root changed |
| `hover` | Tuzi → subscribers | broadcast | `hover` | cursor path changed |
| `yank` | Tuzi → subscribers | broadcast | `yank` | copy/cut state changed |
| `renamed` | Tuzi → subscribers | broadcast | `renamed` | a path was renamed |
| `task-done` | Tuzi → subscribers | broadcast | `task-done` | file task completed |
| custom | any peer → subscribers/peer | either | custom kind | extension message |

Broadcast delivery requires the receiver to declare the matching ability or
the wildcard ability `*`. Direct DDS routing itself does not inspect abilities;
controller performs an ability check before sending, and controlled Tuzi does
the final receiver-side check.

## Handshake messages

```json
{"Hi":{"abilities":["restore-state","set-state"]}}
```

`Hi` must be the first message on a connection. A second `Hi`, a duplicate peer
ID, or a later message with a different sender ID closes that connection.

```json
{"Hey":{"peers":[{"id":701,"abilities":["hover","cd"]},{"id":902,"abilities":["restore-state","set-state"]}]}}
```

`Hey` is emitted whenever the connected peer table changes.

```json
{"Ready":{"token":"random-launch-token"}}
```

`Ready` is sent directly to the configured parent. The enclosing sender is the
new Tuzi peer ID. The controller accepts it only when the token was registered.

## Control messages

```json
{"Open":{"paths":["/project/src/main.rs"]}}
```

`open` is always direct to the parent. It is emitted for ordinary opens when
`dds.open` selects the parent route; interactive opens remain local.

`set-state` is encoded as a custom body on the wire, but is a reserved
controller operation rather than a general `publish` kind:

```json
{"Custom":{"kind":"set-state","data":{"path":"/project","selection":["README.md"]}}}
```

`path` and `selection` are optional. `selection` replaces the current selection
and defaults to an empty list. A controlled Tuzi accepts this operation only
from its saved parent and validates the JSON schema before delivery.

`restore-state` is also encoded as a reserved custom body:

```json
{"Custom":{"kind":"restore-state","data":{"version":1,"active_tab":0,"tabs":[{"cwd":"/project","cursor":"/project/README.md","selection":[],"expanded":["/project/src"]}]}}}
```

The snapshot is the sole source of truth for tab count, order, active tab,
roots, cursors, selections, and expanded directories. Tuzi fully validates it,
builds the replacement trees off-screen, and atomically swaps sessions only
after every lazy listing succeeds. Failure leaves the visible session intact.

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

These five implicit events are private by default. A Tuzi broadcasts only the
kinds present in `config.dds.broadcast`; local in-process subscribers still
receive them.

## Custom messages

```json
{"Custom":{"kind":"plugin-event","data":{"value":1}}}
```

Custom `data` may be any JSON value. Explicit `emit`/`pub` commands bypass the
implicit broadcast allowlist but still require DDS to be enabled. The built-in
names `hi`, `hey`, `ready`, `open`, `cd`, `hover`, `yank`, `renamed`, and
`task-done` are reserved. Controller additionally reserves `set-state` and
`restore-state` for their typed operations.

## Privacy and authentication

- Socket and runtime directory access are restricted to the current OS user.
- The server binds sender identity to one established connection.
- The launch token authorizes only `Ready`; it is not a runtime address.
- Runtime control uses one peer ID at a time.
- Controlled Tuzi verifies that directed control came from its saved parent
  and that it declared the requested state-operation ability.
- `queued` means best-effort submission, not execution acknowledgement.
