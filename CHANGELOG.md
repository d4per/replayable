# Changelog

All notable changes are listed here. Changes to the JSON event format are
always called out, since they affect logs already on disk.

## 0.1.0 – unreleased

First release.

- `model!` declares entities and generates the structs, the change and
  event enums, and the model type.
- `Store` with `create`, `edit`, `update`, `delete`, `try_edit`,
  `try_update`, `get`, `contains`, `count`, `all` and `store[id]`.
- `List` and `Map`, which track their own changes. Adjacent changes in one
  edit are merged into a single `ListOp::Splice`.
- Replay that checks every event and reports corrupt logs as `Error`
  instead of panicking.
- JSON Lines persistence: `write_json`, `write_json_since` for appending,
  `read_json`, `to_json`, `from_json`, and `apply_event` for following
  another store's log.
