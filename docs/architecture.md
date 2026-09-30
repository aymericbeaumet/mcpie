# Architecture

mcpie is one registry with four facades. A source registers its operations once, and the CLI,
MCP, REST, OpenAPI and GraphQL interfaces are generated from that registry at startup. Nothing
per operation exists in any facade.

```
 config (figment) ──► sources::build ──► Registry ──┬─► facade::cli      (clap builder tree)
                                                     ├─► facade::mcp      (rmcp ServerHandler, stdio or /mcp)
                                                     ├─► facade::rest     (axum, /v1 + /openapi.json + /docs)
                                                     └─► facade::graphql  (async-graphql dynamic schema)
```

## Module map

| Path | Role |
|---|---|
| `mcpie/model/` | `Source`, `OperationSpec`, `Registry`, the input projection, cursors, `SourceError`, normalized items |
| `mcpie/config/` | layered configuration, project-file allow-list, `Secret`, credential resolution |
| `mcpie/sources/` | `http.rs` (shared client), one directory per source type, the async `build` function |
| `mcpie/facade/` | `cli/`, `mcp/`, `rest/` (with `openapi.rs`), `graphql/`, `server.rs` (router, guards, serve) |
| `mcpie-cli/main.rs` | the binary: runtime, unlocked stdio handles, exit code |

## The registry contract

A source (`model/source.rs`) is the single stateful object for one configured instance. It owns
its HTTP client, credentials and caches, and answers operations by name:

```rust
fn id(&self) -> &str;                       // instance id, ^[a-z][a-z0-9-]*$
fn kind(&self) -> &'static str;             // type: github, slack, linear, gdrive, gmail, mcp
fn operations(&self) -> &[OperationSpec];   // built once in the constructor
async fn call(&self, op: &str, input: Value, ctx: &CallContext) -> Result<Value, SourceError>;
async fn check(&self, ctx: &CallContext) -> Result<Status, SourceError>;
fn search(&self) -> Option<&dyn SearchProvider>;
```

`model::typed` is the one place JSON crosses into typed Rust: it deserializes the input struct
(`null` counts as `{}`), runs the handler and serializes the output. Input structs derive
`Deserialize`, `JsonSchema` and `#[serde(deny_unknown_fields)]`, so typos are `InvalidInput`.

`OperationSpec` carries the canonical name, title, description, `OperationKind`, a `paginated`
flag (set when the input has a `cursor` property) and the two JSON Schemas, generated with
schemars in draft 2020-12 with every subschema inlined. Credentials resolve lazily on the first
call, so `--help`, `ops` and `completions` work on a fresh install without network access.

The `Registry` validates ids, names and schemas at registration (`model::projection::lint`),
applies the per-instance `tools` allow-list and the read-only policy, exposes `select(&Selection)`
for facades, dispatches `call` with a timeout, and fans `search` out to every source with a
provider.

## Naming

One canonical `snake_case`, verb-first operation name; each facade converts deterministically:

| Facade | `github` / `list_issues` |
|---|---|
| CLI | `mcpie github list-issues --owner o --repo r` |
| MCP tool | `github_list_issues` (source ids never contain `_`, so names split at the first underscore) |
| REST | `POST /v1/sources/github/list_issues`, `GET /v1/sources/github/list_issues?owner=o&repo=r` |
| GraphQL | `{ github { listIssues(owner: "o", repo: "r") } }` |

## The input projection

`model/projection.rs` turns the top-level properties of an input schema into flat typed fields
(`Str`, `I64`, `F64`, `Bool`, `Enum`, `List`, `Json`). The CLI derives flags from it, REST `GET`
parses query strings through it, GraphQL builds arguments from it, and OpenAPI lists parameters
from it. Rules worth knowing:

- `Option<T>` (`type: [.., "null"]`, or `anyOf` with a null branch) is unwrapped.
- `enum` arrays and all-`const` `oneOf`/`anyOf` are enums; documented variants keep descriptions.
- Anything nested is `Json`: a JSON string on the CLI, refused on REST `GET`, the `JSON` scalar in GraphQL.
- The lint rejects reserved field names (`input`, `format`, `config`, `help`, `version`,
  `verbose`, `all`, `timeout`, `set`) and nested arrays, so a new source cannot break a facade.

## Pagination and cursors

Every paginated operation accepts `limit` and `cursor` and returns `{ items, next_cursor }`.
Cursors (`model/cursor.rs`) are base64url of `{"v":1,"s":source,"o":operation,"p":state}`; the
decoder rejects a cursor issued by another source or operation. The state never contains a URL,
so a crafted cursor cannot redirect a request or its bearer token. GitHub stores a page number
parsed from the `Link` header, Slack and Google wrap their opaque tokens, Linear wraps Relay
cursors, and Slack search stores a page number.

## Errors

One `SourceError` (`model/error.rs`) with a stable machine `code` in every facade:

| Variant | CLI exit | MCP | HTTP | code |
|---|---|---|---|---|
| `InvalidInput` | 2 | in-band tool error | 400 | `invalid_input` |
| `UnknownSource` / `UnknownOperation` | 2 | protocol error (unknown tool) | 404 | `unknown_operation` |
| `NotConfigured` | 1 | in-band | 503 | `not_configured` |
| `Auth` | 1 | in-band | 502 | `upstream_auth` |
| `RateLimited { retry_after }` | 1 | in-band | 429 + `Retry-After` | `rate_limited` |
| `NotFound` | 1 | in-band | 404 | `not_found` |
| `Upstream { status }` | 1 | in-band | 502 | `upstream` |
| `Timeout` | 1 | in-band | 504 | `timeout` |
| `Transport` | 1 | in-band | 502 | `transport` |
| `Unsupported` | 1 | in-band | 501 | `unsupported` |
| `Internal` | 1 | protocol error | 500 | `internal` |

HTTP 401 is reserved for mcpie's own bearer token and 421 for the Host guard; upstream statuses
are never forwarded. REST bodies are `{"error": {"code", "message", "source", "retry_after"?,
"request_id"}}`; GraphQL puts `code` and `source` in `extensions`. In-band MCP errors let the
model read the message and correct its call.

## Normalized search

`model/normalized.rs` defines `Item { kind, source, id, title, snippet, url, author, updated_at,
fetch, raw }`. Sources implement `SearchProvider` in their `normalize.rs`; the registry fans out
with a per-source timeout, merges newest first, truncates to `limit`, strips `raw` unless asked,
and reports per-source failures in `errors` next to the items. Search returns references:
`fetch` names the native operation and input that return the full object.

## Read-only policy

v1 registers only `OperationKind::Read`. Write operations are hidden by `select` and refused by
`call`. Every source has a raw `request` escape hatch limited to reads: GitHub and Google only
`GET`, Slack an explicit list of read methods, Linear refuses documents containing `mutation`.
Custom MCP tools count as read-only only with `readOnlyHint: true` or a `trusted_read_only` match.

## Security model of the local server

- Binds `127.0.0.1` by default and refuses a non-loopback bind without `server.token` (or
  `--insecure-no-auth`).
- A router-wide guard rejects any `Host` other than loopback names and the bound IP, so a
  DNS-rebinding page cannot reach the server; rmcp's own host check is disabled in favour of it.
- With `server.token` set, every request needs `Authorization: Bearer` except `/healthz`,
  `/openapi.json`, `/docs` and `GET /graphql`. Comparison is constant-time.
- `x-request-id` is propagated or generated and echoed on every response.
- A checked-in `.mcpie.toml` may only carry `enabled`, `tools` and `default_*` keys per source.
- `Secret` never prints; `token_command` output is never logged; the HTTP client never logs headers.
- `mcpie mcp` writes nothing but protocol frames to stdout; logs go to stderr.
