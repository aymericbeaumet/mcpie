# mcpie agent guide

- Run `make check` after material changes.
- Every commit message MUST follow the latest published
  [Conventional Commits specification](https://www.conventionalcommits.org/).
- Keep reusable logic in `mcpie` and process I/O in `mcpie-cli`.
- One registry, many facades: a source registers operations once (spec table plus `call`
  match); the CLI, MCP, REST, OpenAPI and GraphQL interfaces are generated from that registry and
  must not grow per-operation code. See `docs/architecture.md`.
- New source types follow `docs/adding-a-source.md`: flat, documented input structs with
  `deny_unknown_fields`, verb-first snake_case names, opaque cursors, a read-only `request`
  escape hatch, `check`, and httpmock tests. The registry lint rejects reserved input names.
- v1 is read-only: never register `OperationKind::Write` operations or widen a raw `request`
  beyond reads.
- Secrets never print: use `config::Secret`, never log headers or `token_command` output, and keep
  `mcpie mcp`'s stdout for protocol frames.
- Configuration semantics live in `docs/configuration.md`; update it with every new key.
- Distribute only through GitHub Releases and mise for Linux, macOS, and Windows on arm64 and
  amd64. Do not use Homebrew.
- `cargo fmt`, `cargo clippy --workspace --all-targets -- --deny warnings`, and
  `cargo test --workspace --all-targets` must stay clean.
- Do not add `CLAUDE.md` or other client-specific instruction files.
