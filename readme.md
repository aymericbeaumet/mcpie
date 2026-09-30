# mcpie [![release](https://github.com/aymericbeaumet/mcpie/actions/workflows/release.yml/badge.svg)](https://github.com/aymericbeaumet/mcpie/actions/workflows/release.yml)

Query Slack, GitHub and other sources through one CLI, MCP server, REST API or GraphQL endpoint.

mcpie keeps a single registry of sources and their read-only operations, and generates every
interface from it. Configure a source once and it is available from your shell, from an AI agent
over MCP, and from any program over HTTP.

## Install

```shell
mise use -g github:aymericbeaumet/mcpie
```

Prebuilt binaries for Linux, macOS and Windows on `arm64` and `amd64`, with checksums, are on the
[GitHub Releases](https://github.com/aymericbeaumet/mcpie/releases) page.

## Getting Started

GitHub works out of the box when you are logged in with `gh`; otherwise set a token:

```shell
mcpie config init                                    # ~/.config/mcpie/config.toml, secrets stay out of git
export MCPIE_SOURCES__GITHUB__TOKEN=ghp_...          # or --set sources.github.token=ghp_...
```

Check what is connected and what it can do:

```console
$ mcpie sources
ID      TYPE    STATUS  IDENTITY        CREDENTIAL     MESSAGE
github  github  ok      aymericbeaumet  gh auth token

$ mcpie ops github
SOURCE  OPERATION            INPUTS                                          DESCRIPTION
github  get-viewer                                                           Return the authenticated user.
github  list-repos           owner, type, sort, direction, limit, cursor     List repositories of the authenticated user, a user or an organization.
github  list-issues          owner, repo, state, labels, assignee, ...       List issues of a repository; pull requests are included and carry a pull_request key.
github  get-issue            owner, repo, number*                            Return one issue by number, with its body.
github  search-code          query*, limit, cursor                           Search file contents across GitHub with the code search syntax.
...
```

Every operation is a subcommand with typed flags; results are JSON on stdout:

```console
$ mcpie github list-issues --owner aymericbeaumet --repo bonsai --state all --limit 2 | jq '.items[] | {number, title, state}'
{"number":5,"title":"fix: share and clarify bundled agent guidance","state":"closed"}
{"number":4,"title":"feat: unify project and session navigation","state":"closed"}

$ mcpie github get-file-content --owner aymericbeaumet --repo bonsai --path readme.md | jq -c '{name, encoding, size}'
{"name":"readme.md","encoding":"utf-8","size":15862}
```

Paginated operations return `next_cursor`; pass it back as `--cursor`, or use `--all`.
Any operation also accepts `--input '{...}'`, `--input .json` or `--input -` (stdin), and
`mcpie describe github list-issues` prints the full schema.

## Sources

| Source | Type | Credentials |
|---|---|---|
| GitHub | `github` | `token`, `GITHUB_TOKEN`/`GH_TOKEN`, or `gh auth login` |
| Slack | `slack` | `token` or `SLACK_TOKEN` (`xoxp-` user tokens can search, `xoxb-` bot tokens cannot) |
| Linear | `linear` | `token` or `LINEAR_API_KEY` |
| Google Drive | `gdrive` | `mcpie auth gdrive` with your own OAuth client, or `token_command = "gcloud auth application-default print-access-token"` |
| Gmail | `gmail` | `mcpie auth gmail`, or the same `gcloud` command |
| Any MCP server | `mcp` | whatever the server needs |

Every source has a `request` operation for raw read-only calls, and every list operation
paginates the same way. Instances are named by their config key, so two GitHub hosts or several
MCP servers can coexist:

```toml
[sources.ghe]
type = "github"
base_url = "https://github.example.com/api/v3"
token_command = "op read op://work/ghe/token"

[sources.notion]
type = "mcp"
command = ["npx", "-y", "/notion-mcp-server"]
env = { NOTION_TOKEN = "ntn_..." }
# tools without a readOnlyHint stay hidden unless you vouch for them:
trusted_read_only = ["API-get-*", "API-post-search"]
```

A checked-in `.mcpie.toml` in a repository may set `enabled`, `tools` and `default_*` keys for
each source (a default owner and repo, for instance) but never credentials or commands.

## AI agents (MCP)

The same registry is an MCP server. With Claude Code:

```shell
claude mcp add mcpie -- mcpie mcp
```

For any other client, run `mcpie mcp` over stdio:

```json
{ "mcpServers": { "mcpie": { "command": "mcpie", "args": ["mcp"] } } }
```

Every read operation becomes a tool named `<source>_<operation>` (`github_list_issues`), all
annotated read-only, plus `search` across every source. Sources without credentials are left
out. Options: `--sources github,slack`, `--tools 'list_*,github.get_issue'`,
`--exclude-tools '*_request'`, and `--mode meta` to expose only four tools
(`list_operations`, `describe_operation`, `call_operation`, `search`) when context is tight.

## HTTP: REST, GraphQL, OpenAPI and MCP over HTTP

```shell
mcpie serve                                   # http://127.0.0.1:7878
curl localhost:7878/v1/sources
curl 'localhost:7878/v1/sources/github/list_issues?owner=acme&repo=widgets&limit=5'
curl -X POST localhost:7878/v1/sources/github/search_issues -d '{"query":"repo:acme/widgets is:open"}'
curl 'localhost:7878/v1/search?query=release&kinds=issue,message'
open http://localhost:7878/docs                # OpenAPI 3.1 lives at /openapi.json
```

The same process serves GraphQL at `/graphql` (GraphiQL in the browser, `POST` for queries),
with one object per source, one field per operation and typed arguments:

```graphql
{
  github { listIssues(owner: "acme", repo: "widgets", state: "open", limit: 5) }
  slack { searchMessages(query: "release in:#eng", limit: 3) }
  search(query: "incident 42", kinds: ["issue", "message"])
}
```

MCP is served over streamable HTTP at `/mcp`. Errors share one shape,
`{"error": {"code", "message", "source", "request_id"}}`, with the same codes as the CLI and MCP.

The server is meant for one machine: it binds `127.0.0.1` by default, rejects requests whose
`Host` header is not local (a DNS-rebinding guard), and refuses a non-loopback bind unless
`server.token` is set, in which case every request needs `Authorization: Bearer <token>`.

## Documentation

- [Architecture](docs/architecture.md): the registry, the input projection, cursors, errors, the
  normalized search layer and the security model of the local server.
- [Configuration](docs/configuration.md): every key, credential resolution per source, Google
  OAuth, custom MCP servers and the project file.
- [Adding a source](docs/adding-a-source.md): the checklist a new source type follows.

## Development

```shell
mise install       # pinned toolchain
make check         # fmt, clippy, tests, docs: the same checks CI runs
make run ARGS='--help'
```

## License

[MIT](LICENSE)
