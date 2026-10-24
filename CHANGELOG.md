# Changelog

All notable changes are listed here. Changes to the JSON event format are
always called out, since they affect logs already on disk.

## 0.1.0 – unreleased

First release.

- `model!` declares entities and generates the structs, the change and
  event enums, and the model type.
- `Store` with `create`, `edit`, `update`, `try_edit`, `try_update`,
  `get`, `contains`, `count`, `all` and `store[id]`.
- No explicit delete. `Store::pin` and `unpin` mark the entities to keep,
  and `Store::collect` deletes every entity they can't reach, cycles
  included, logging a `DeleteX` event for each. `Weak<E>` field types
  (also in `List` and `Map`) don't keep anything, and are cleared, with
  logged events, when what they point to is collected. `Value::refs` lets
  your own value types report the ids they hold. Event format: new
  `"PinX": id` and `"UnpinX": id` events.
- `List` and `Map`, which track their own changes. Adjacent changes in one
  edit are merged into a single `ListOp::Splice`.
- Replay that checks every event and reports corrupt logs as `Error`
  instead of panicking.
- JSON Lines persistence: `write_json`, `write_json_since` for appending,
  `read_json`, `to_json`, `from_json`, and `apply_event` for following
  another store's log.
- Schema changes: fields with `= default`, renames with `(was old)`, and
  removed fields and entity types declared with `#![removed(..)]`. Unknown
  names in a log are an error.
- `create` logs the whole value as one event, so later changes to a
  default don't change history.
- `Value::check` rejects NaN and infinite floats, debug builds check that
  every logged change survives a trip through JSON, and `Store::verify`
  compares a replay with the live state.
- `Store::as_of(n)` rebuilds the state after the first `n` events, for
  example to see something as it was before it was deleted.
- `List` can be indexed with ranges (`list[a..b]`), like a `Vec`.
- Every event is logged with its time, a `Timestamp` to the millisecond,
  written first on each line as ISO 8601 UTC:
  `{"at":"2026-10-24T09:15:02.123Z","CreateUser":[..]}`. The time is
  required when reading a log. `Store::timestamps`, `Store::as_of_time`,
  `Store::apply_event_at` and `Store::with_clock` (for tests). Times never
  go backwards in a log.
- A compact binary log format, usually a quarter of the size of the JSON:
  `Store::write_binary`, `write_binary_since`, `read_binary`, `to_binary`
  and `from_binary`. It's self-describing like the JSON, so model changes
  work the same way, and appending doesn't repeat names already in the
  file. Errors in it are `Error::Binary`. `Timestamp` is written as a
  number in non-human-readable serde formats.
