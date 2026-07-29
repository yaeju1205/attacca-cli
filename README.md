# attacca-cli

Chat with [Attacca](https://attacca.cc) agents from your terminal, with token-by-token streaming and
local computer access — over [Zyris](https://github.com/attacca-cc/zyris), Attacca's node protocol.

The CLI is a Zyris node. It dials one websocket, announces the `terminal` and `file_io` capabilities
so your agents can drive this machine, and consumes the `attacca_api` capability the server announces
back on the same connection to list sessions, post messages, and stream turns.

## Quick start

```bash
cargo run
```

There is nothing to configure against the hosted deployment — no URL, no key. On first run the node
enrolls itself over the OAuth 2.0 Device Authorization Grant (RFC 8628): it prints a short code and
waits.

```
--------------------------------------------------------------
  Authorize this node

  1. Open        https://attacca.cc/settings/zyris/device
  2. Enter code  WXQR-7KBD

  Waiting for approval. This code expires in 10 minutes.
--------------------------------------------------------------
```

Type that code into Attacca on any device with a browser, choose which scopes to grant, and press
Authorize. The credential is written to `~/.config/zyris/` (mode `0600`) and refreshes itself, so
later runs connect straight into the TUI with nothing printed.

The code is printed **before** the TUI opens, deliberately — an alternate screen would swallow it.

> An `atk_` REST API key does **not** work here. Zyris authenticates with `znt_` node tokens, and
> pasting an API key into `ZYRIS_NODE_TOKEN` reports that rather than failing with a bare 401.

## Re-login

`/login` authorizes this node again without restarting. It drops the TUI so the code is visible,
clears the stored credential so the flow enrolls rather than quietly refreshing the grant already on
disk, waits for your approval, then restores the screen and drops the connection so the runner
redials with the new credential.

That last step is the reason `/login` exists rather than just `/logout`: a `Runner` reads its
credential only when it dials, and a `DeviceGrant` caches what it obtained for the access token's
full hour — so re-enrolling alone would leave the process happily using the old token.

Reach for it when a credential was enrolled with narrower scopes than the CLI needs. A grant made by
some other node — `zyris-hello` asks for `agents:read` alone — is reused as-is, because a refresh
never re-requests scopes. `/whoami` shows what you actually have.

## Scopes

The node asks for `agents:read`, `projects:read`, `sessions:read`, `sessions:write`, and
`events:read`. You may grant fewer at the approval screen; `/whoami` shows what was actually granted,
and a call that needs more comes back as a `forbidden_scope` notice. `events:read` is the one that
matters for streaming — without it there is no `turn_events`, and so no live output at all.

## Streaming

Each open session holds one `attacca_api.turn_events` subscription. Token deltas grow the assistant
card in place (marked with a `▌` while it fills), durable events settle it to the canonical text, and
the subscription resumes from the last cursor it saw — so a dropped connection reconnects and picks
up mid-turn without repeating anything. Opening a session replays its log from cursor 0, which is
also how history loads.

### What the sidebar updates live, and what it can't

`turn_events` is the only stream `attacca_api` v1 declares, and it is **per-session** — it takes a
session id. There is no account-wide event stream to subscribe to, so the sidebar is live for the
session you have open and event-driven, not live, for the rest:

- **Open session, pushed instantly** — its running dot follows `Status` frames, and its name updates
  the moment an event carries a new `title` (which is how a server-side auto-title after the first
  turn shows up).
- **The rest of the list, refreshed when a turn ends** — a turn ending is the one push signal that
  says "the list may have moved on", so `list_sessions` re-runs then. Sessions created or renamed in
  another client appear at that point, not the instant they change.
- **Never on a timer.** Polling is what this rewrite removed, and adding it back for the sidebar
  would undo that.

Run `/tools` to see what a deployment actually announces. The announced tool list is never compared
against `zyris-attacca`'s declaration, so a newer server may offer something account-wide that this
CLI could subscribe to instead — if it appears there, wiring it up is small.

### History

Following a session takes two calls, and the difference in how they read `after` is the whole reason
for both. `session_history` with no `after` is the entire timeline; `turn_events` with no `after` is
live frames only. So opening a session reads its history, then the stream takes over from the last
cursor — and each reconnect repeats the pair, the catch-up read closing whatever gap the disconnection
left. A cursor already applied is skipped, so a re-read never duplicates the conversation.

If a session opens with its history missing but a `history:` notice absent, the events arrived and the
kind mapping missed them: run with `ATTACCA_DEBUG_EVENTS=1` to see the actual kinds, then adjust
`classify` in `src/app.rs`.

## Local computer access

Your agents call the capabilities this node announces, as `zyris__<slug>__terminal__exec` and
`zyris__<slug>__file_io__read` (the slug is on the node card in the dashboard).

| Capability | Tools |
|---|---|
| `terminal` | `open`, `write`, `resize`, `close`, `exec` |
| `file_io` | `stat`, `list`, `read`, `write`, `remove`, `mkdir` |

Both are rooted at `ATTACCA_FILE_ROOT`, defaulting to the directory you launched from. That root is
the **working directory**, not a sandbox: a relative path resolves against it, `exec` starts each
command there unless given a `cwd`, and `zyris-caps` resolves paths with a shared `resolve_under` that
honours absolute paths as given and lets `..` climb out.

**So these reach anything your user account can, and run without per-call confirmation.** That is what
auto-approval means. `ATTACCA_NO_TERMINAL=1` serves the file half only.

### The node brief

Sessions this CLI creates carry a `preamble`: system instructions for that session alone, appended to
the agent's own on every turn. It names the node, lists the capabilities it actually announces, gives
the working directory both tools are rooted at, and tells the agent to treat "this file" and "run the
tests" as meaning *that* machine. It spells out that relative paths land in that directory while
absolute ones reach the wider filesystem, and asks the agent to propose destructive commands rather
than just running them, since nothing here prompts for approval.

Being a preamble rather than a prefix on the first message means it applies to the whole conversation
and never appears in the transcript. Sessions you open from the sidebar are left alone — they carry
whatever preamble they were created with.

Adjusting the wording means editing `NodeBrief::preamble` in `src/brief.rs`.

### Titles

New sessions are created **untitled**, on purpose. Attacca's title agent names a session from its
first message, in that message's own language, and a title supplied at creation is permanent and opts
the session out of that for good. Sessions from older builds of this CLI are titled `attacca-cli` for
exactly that reason; the sidebar shows them as `untitled`.

## Commands

| Command | Description |
|---|---|
| `/help` | Show help |
| `/new` | Create a new session |
| `/cancel` | Stop the running turn |
| `/login` | Authorize this node again, without restarting |
| `/whoami` | Identity and granted scopes |
| `/logout` | Forget this node's credential |
| `/tools` | What the server announces on this connection |
| `/sessions` | Focus the sidebar |
| `/exit` | Quit |

## Configuration

Zyris variables, read by `RunConfig::from_env`:

| Variable | Default | Notes |
|---|---|---|
| `ZYRIS_SERVER_URL` | `wss://attacca.cc/api/zyris/v1/ws` | A local server is `ws://127.0.0.1:8080/zyris/v1/ws`. Enrollment endpoints are derived from this, so a node cannot enroll against one deployment and connect to another. |
| `ZYRIS_NODE_TOKEN` | *unset* | A static `znt_` token, for anything provisioned without a human. Skips enrollment. |
| `ZYRIS_NODE_TOKEN_FILE` | *unset* | Read the token from a file, re-read every dial so rotation needs no restart. |
| `ZYRIS_NODE_NAME` | this machine's hostname | The name proposed at enrollment; the approving user may change it. |
| `ZYRIS_SCOPES` | the five above | Comma-separated scopes to request. Setting it wins over the built-in list. |
| `ZYRIS_PROFILE` | `default` | Names the credential file, so one machine can hold separate identities. |
| `ZYRIS_CONFIG_DIR` | XDG default | Where credentials live. |

Attacca-specific:

| Variable | Default | Notes |
|---|---|---|
| `ATTACCA_AGENT` | first agent listed | Agent UUID new sessions are created with. |
| `ATTACCA_PROJECT` | account default | Project name or UUID for new sessions. |
| `ATTACCA_FILE_ROOT` | where you launched from | Working directory for `file_io` and `terminal`. Relative paths resolve against it; absolute paths and `..` still reach outside. |
| `ATTACCA_NO_TERMINAL` | *unset* | Set to omit the `terminal` capability. |
| `ATTACCA_LOG` | *unset* | Log to this file (`attacca-cli.log` if set but empty). Without it nothing is logged — a subscriber writing to stdout would scribble over the TUI. |
| `ATTACCA_DEBUG_EVENTS` | *unset* | Show unrecognised session events as dim lines, for finding out what a deployment emits. |

## Install

```bash
cargo install --path .
attacca
```
