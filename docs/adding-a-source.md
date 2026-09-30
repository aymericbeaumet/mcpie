# Adding a source

A source type is one directory under `mcpie/sources/` plus one match arm in `sources::build`.
Everything else (CLI subcommands, MCP tools, REST routes, OpenAPI, GraphQL fields) is derived.

## Checklist

1. **Directory**: `mcpie/sources/<type>/` with `mod.rs` (the `Source` impl, the spec table and the
   `call` match), `client.rs` (upstream endpoints and error mapping on top of `sources/http.rs`),
   `types.rs` (input structs) and `normalize.rs` (the `SearchProvider`, when the upstream can search).
2. **Settings**: an `Extra` struct with `#[serde(default, deny_unknown_fields)]` for type-specific
   keys under `[sources.<id>]`. Common keys (`enabled`, `token`, `token_command`, `base_url`,
   `tools`, `timeout_seconds`) are handled by `config::SourceConfig`; read yours with `config.extra()`.
3. **Credentials**: resolve lazily through `config::resolve(CredentialSpec { token, token_command,
   env, .. })` cached in a `tokio::sync::OnceCell`, and return `SourceError::NotConfigured` with a
   message that names the setting or command that fixes it.
4. **Inputs**: one struct per operation deriving `Deserialize`, `JsonSchema` and
   `#[serde(deny_unknown_fields)]`; every field gets a doc comment (it becomes the schema
   description and the CLI help). Keep inputs flat: scalars, `Option<scalar>`, `Vec<scalar>`, unit
   enums, or a `Map<String, Value>` for free-form objects. Do not use the reserved names listed in
   `model::projection::RESERVED_NAMES`. Paginated operations take `limit: Option<u32>` and
   `cursor: Option<String>` and return `Page<Value>`.
5. **Spec table**: `OperationSpec::read::<Input, Output>(name, title, description)` for each
   operation, verb-first snake_case names (`list_issues`, `get_issue`, `search_issues`, `request`).
6. **Dispatch**: a `match` in `call` using `typed(input, |i: Input| self.method(i)).await`. Add
   the "every operation is dispatched" test (see `mcpie/tests/github.rs`) so the table and the
   match cannot drift.
7. **Pagination**: wrap upstream tokens with `model::cursor::encode(&self.id, operation, &state)`
   and decode with `cursor::decode`; clamp `limit` to the endpoint maximum.
8. **Errors**: map upstream statuses and bodies to `SourceError` in `client.rs`; never forward raw
   statuses. Rate limits become `RateLimited { retry_after }`.
9. **Raw escape hatch**: a read-only `request` operation, restricted to `GET` or an explicit list
   of read methods.
10. **`check`**: a cheap identity probe returning `Status { identity, token_kind, scopes,
    credential, warnings, unavailable }`; `credential` is `provenance.to_string()`.
11. **Search**: implement `SearchProvider` in `normalize.rs` mapping hits into `Item` with a `fetch`
    reference, and return `Some(self)` from `Source::search`. Respect `query.kinds` to skip
    irrelevant calls.
12. **Wire it**: add the type to `KNOWN_TYPES` and `build` in `mcpie/sources/mod.rs`, to
    `config::BUILTIN_TYPES` when it should have a default instance, and to `config::TEMPLATE`.
13. **Tests**: `mcpie/tests/<type>.rs` against `httpmock` covering headers, pagination, error
    mapping, the raw request guard and `check`; unit tests for `normalize.rs` mappings.
14. **Docs**: a row in the readme's source table and, if the credential story is unusual, a
    section in `docs/configuration.md`.

Run `make check`; the registry lint runs inside the tests and fails registration for any schema
a facade could not expose consistently.
