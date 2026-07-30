# attacca-cli

Chat with [Attacca](https://attacca.cc) agents from your terminal, with token-by-token streaming —
and let them work on this machine.

The client runs as a **Zyris node**: one websocket to Attacca, over which it announces the
capabilities the agent may call (`file_io`, `terminal`) and consumes the `attacca_api` capability
Attacca announces back. Replies arrive as they are generated rather than being polled for.

## Quick start

```bash
cargo run
```

The first run enrolls this machine. It prints a short code and a URL — approve it in a browser and
the credential is written to `~/.config/zyris` with mode 0600. There is nothing to copy and paste.

For a headless or CI run, provision a node token instead and skip enrollment entirely:

```bash
export ZYRIS_NODE_TOKEN=znt_...      # or ZYRIS_NODE_TOKEN_FILE=/run/secrets/zyris
cargo run
```

`ATTACCA_API_KEY` is no longer used. The client authenticates as a node, not with an account key.

## What the agent can do on this machine

`file_io` (stat, list, read, write, remove, mkdir) and `terminal` (`exec`, plus an interactive PTY)
are announced as real, schema'd tools.

> **They run immediately, with no confirmation step.** The agent can write files and run shell
> commands as your user, without being asked. `ATTACCA_FILE_ROOT` sets where *relative* paths land
> — it is not a sandbox: absolute paths are honoured as given and `..` climbs out of it. Anything
> your account can reach is reachable through these tools.

To serve the file half alone, set `ATTACCA_NO_TERMINAL=1`. The brief the agent is given is built
from the same decision, so it is never told about a capability this node does not actually serve.

## Commands

| Command | Description |
|---------|-------------|
| `/help` | Show help |
| `/exit` | Quit (`/quit`, Ctrl+C) |
| `/new` | Start a fresh session |
| `/sessions` | Focus the sidebar and refresh it |
| `/cancel` | Stop the running turn |
| `/login` | Authorize this node again |
| `/logout` | Forget this node's credential |
| `/whoami` | Identity and granted scopes |
| `/usage` | Account and session usage (`/credits`, `/me`) |
| `/tools` | What the server announces on this connection |

Keys: `Enter` sends, `Shift+Enter` inserts a newline (`Alt+Enter` and `Ctrl+J` also work),
`Tab` toggles the sidebar or cycles autocomplete, `↑↓` scrolls the chat, `Ctrl+↑/↓` scrolls the
input box.

## Configuration

All optional. Zyris:

| Variable | Default |
|---|---|
| `ZYRIS_SERVER_URL` | `wss://attacca.cc/api/zyris/v1/ws` |
| `ZYRIS_NODE_TOKEN` | — a `znt_` token; set it to skip enrollment |
| `ZYRIS_NODE_TOKEN_FILE` | — re-read on every dial, so rotation needs no restart |
| `ZYRIS_NODE_NAME` | this machine's hostname |
| `ZYRIS_SCOPES` | `agents:read projects:read sessions:read sessions:write events:read` |
| `ZYRIS_PROFILE` | `default` |
| `ZYRIS_CONFIG_DIR` | `~/.config/zyris` |

Attacca:

| Variable | Default |
|---|---|
| `ATTACCA_AGENT` | first agent on the account; accepts a name or a UUID |
| `ATTACCA_PROJECT` | the account's default project; accepts a name or a UUID |
| `ATTACCA_FILE_ROOT` | the current directory |
| `ATTACCA_NO_TERMINAL` | unset — set it to announce `file_io` only |
| `ATTACCA_LOG` | unset — a path to log to; without it nothing is logged, because stdout is the TUI |
| `ATTACCA_DEBUG_EVENTS` | unset — render event kinds the client does not recognise |
| `ATTACCA_HIDE_REASONING` | unset — drop "thinking" cards from the transcript |

`ZYRIS_SCOPES` is worth knowing about in one case: without `events:read` there is no turn feed at
all, and the only symptom is a chat that never produces anything. The client checks the granted
scopes on connect and names anything missing.

### Session titles and projects

Sessions are created untitled on purpose — Attacca names a session from its first message, and a
title set at creation is permanent and suppresses that.

`ATTACCA_PROJECT` and `ATTACCA_AGENT` each take a name or a UUID. A value matching neither is
reported rather than silently replaced with the default, so a typo does not quietly file a session
somewhere surprising.

## If the transcript comes up blank

The vocabulary of durable event kinds is the server's, and the client maps it onto cards by
substring (`classify` in `src/app.rs`). A deployment emitting kinds none of those match would open a
session showing nothing, with no error. `ATTACCA_DEBUG_EVENTS=1` renders every unrecognised event so
you can see what is actually arriving.

## Building

```bash
cargo install --path .
attacca
```

The three `zyris` crates are not published, so they come from git — with a `[patch]` in
`Cargo.toml` pointing at a sibling checkout of
[`attacca-cc/zyris`](https://github.com/attacca-cc/zyris). **A clean clone without `../zyris` cannot
resolve that**; drop the `[patch]` section to build purely from git. It is there because the
`session_usage` tool this client calls is newer than the published capability.

`reqwest` is no longer a direct dependency but returns transitively at 0.13 through `zyris`'s
`enroll` feature, which is what implements the device grant.
