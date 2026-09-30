# mcpie agent guide

- Run `make check` after material changes.
- Every commit message MUST follow the latest published
  [Conventional Commits specification](https://www.conventionalcommits.org/).
- Keep reusable logic in `mcpie` and process I/O in `mcpie-cli`.
- One registry, many facades: a source registers operations once (spec table plus `call`
  match); the CLI, MCP, REST and GraphQL interfaces are generated from that registry and must
  not grow per-operation code.
- Distribute only through GitHub Releases and mise for Linux, macOS, and Windows on arm64 and
  amd64. Do not use Homebrew.
- `cargo fmt`, `cargo clippy --workspace --all-targets -- --deny warnings`, and
  `cargo test --workspace --all-targets` must stay clean.
- Do not add `CLAUDE.md` or other client-specific instruction files.
