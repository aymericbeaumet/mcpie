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

## Development

```shell
mise install       # pinned toolchain
make check         # fmt, clippy, tests, docs: the same checks CI runs
make run ARGS='--help'
```

## License

[MIT](LICENSE)
