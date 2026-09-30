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

```shell
mcpie --version
```

More to come as sources and interfaces land.

## Development

```shell
mise install       # pinned toolchain
make check         # fmt, clippy, tests, docs: the same checks CI runs
make run ARGS='--help'
```

## License

[MIT](LICENSE)
