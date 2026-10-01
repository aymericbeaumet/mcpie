# Configuration

Layers, lowest to highest precedence, all using the same keys:

1. compiled defaults
2. the user file: `$XDG_CONFIG_HOME/mcpie/config.toml`, else `~/.config/mcpie/config.toml`
   (`mcpie config path` prints it, `mcpie config init` writes a commented template, mode 0600)
3. the nearest `.mcpie.toml` up from the working directory (team defaults, see below)
4. `MCPIE_*` environment variables, `__` separating sections: `MCPIE_SOURCES__GITHUB__TOKEN`
5. `--set key=value` flags, repeatable, TOML-typed values: `--set http.timeout_seconds=5`

`mcpie config show` prints the effective configuration with secrets redacted. `--config PATH`
selects another user file; `MCPIE_CONFIG` does the same.

## Keys

```toml
[server]
bind = "127.0.0.1:7878"          # mcpie serve; non-loopback binds require a token
token = ""                       # bearer token for every HTTP request except /healthz, /openapi.json, /docs, GET /graphql

[http]
timeout_seconds = 30             # per call; --timeout overrides
max_in_flight_per_source = 4     # concurrency limit shared by every facade

[sources.<id>]
type = "github"                  # defaults to <id> for built-in types: github, slack, linear, gdrive, gmail; or "mcp"
enabled = true
token = ""                       # credential, or:
token_command = ""               # a command printing the credential (gh, op, pass, gcloud, ...)
base_url = ""                    # override the API host (GitHub Enterprise, tests)
tools = ["*"]                    # glob allow-list over operation names, applied to every facade
timeout_seconds = 30             # per-instance override
```

Instance ids match `^[a-z][a-z0-9-]*$`. Several instances of one type may coexist.

## Credentials per source

| Type | Resolution order | Notes |
|---|---|---|
| `github` | `token`, `token_command`, `GITHUB_TOKEN`, `GH_TOKEN`, `gh auth token` | `default_owner`, `default_repo` fill omitted inputs |
| `slack` | `token`, `token_command`, `SLACK_TOKEN`, `SLACK_USER_TOKEN`, `SLACK_BOT_TOKEN` | `xoxp-` user tokens can `search_messages`; `xoxb-` bot tokens cannot |
| `linear` | `token`, `token_command`, `LINEAR_API_KEY`, `LINEAR_TOKEN` | `default_team` fills omitted `team` inputs |
| `gdrive`, `gmail` | `token`, `token_command`, `oauth.refresh_token`, `GOOGLE_OAUTH_ACCESS_TOKEN` | see below |
| `mcp` | `token`/`token_command` become `Authorization: Bearer` for `url` servers | see below |

### Google

Two ways to authenticate:

- `token_command = "gcloud auth application-default print-access-token"` after
  `gcloud auth application-default login --scopes=openid,https://www.googleapis.com/auth/drive.readonly,https://www.googleapis.com/auth/gmail.readonly`.
- Your own OAuth client: in Google Cloud create an OAuth client of type "Desktop app" with the
  Drive and Gmail APIs enabled, then run `mcpie auth gdrive --client-id ... --client-secret ...`
  (and `mcpie auth gmail`). The browser opens the consent page, the code comes back on a random
  `127.0.0.1` port, and the refresh token is stored under `[sources.<id>.oauth]` in the user
  file. Access tokens are refreshed and cached in memory; mcpie ships no client id of its own.

### Custom MCP servers

```toml
[sources.notion]
type = "mcp"
command = ["npx", "-y", "@notionhq/notion-mcp-server"]   # stdio server
env = { NOTION_TOKEN = "ntn_..." }
cwd = "/path"                                            # optional
# or a streamable HTTP server:
# url = "https://mcp.example.com/mcp"
# headers = { X-Org = "acme" }
# token = "..."                                          # sent as Authorization: Bearer
trusted_read_only = ["API-get-*"]                        # tools without readOnlyHint to expose anyway
connect_timeout_seconds = 15
```

Tools are discovered once at startup. Tools annotated `readOnlyHint: true` become operations;
others stay hidden unless matched by `trusted_read_only`. A server that cannot be reached shows as
an error in `mcpie sources` without stopping the rest.

## The project file

A repository may check in `.mcpie.toml` with team defaults:

```toml
[sources.github]
default_owner = "acme"
default_repo = "widgets"
tools = ["list_*", "get_*", "search_*"]

[sources.slack]
enabled = false
```

Only `sources.<id>.enabled`, `sources.<id>.tools` and `sources.<id>.default_*` keys are accepted.
Anything else (`token`, `token_command`, `base_url`, `command`, `server.*`, `http.*`) is a hard
error naming the file and key, so a checked-in file can never run commands, redirect requests or
carry secrets.

## Exposure filters for `mcp` and `serve`

By default both hide enabled sources whose credentials do not resolve. `--sources a,b` picks
sources explicitly, `--all` exposes every enabled source, `--tools 'list_*,github.get_issue'` and
`--exclude-tools '*_request'` filter operations (globs match `operation` or `source.operation`),
and `--mode meta` replaces per-operation MCP tools with `list_operations`, `describe_operation`,
`call_operation` and `search`.
