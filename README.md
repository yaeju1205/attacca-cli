# attacca-cli

Chat with [Attacca](https://attacca.cc) agents from your terminal — with local computer access via tools.

## Quick start

```bash
cp .env.example .env   # edit your API key
cargo run              # interactive mode
cargo run -- "hello"   # one-shot mode
```

## Setup

1. Get an API key at **https://attacca.cc → Settings → API keys**
2. Create `.env`:
   ```
   ATTACCA_API_KEY=atk_your_key_here
   ```

Or set the env var directly:
```bash
export ATTACCA_API_KEY=atk_...
cargo run
```

## Bridge mode

The agent can access your local computer via tool calls wrapped in ` ```attacca-tool ` blocks. Every tool requires your approval before execution.

| Tool | What it does |
|------|-------------|
| `read_file` | Read a text file |
| `write_file` | Write a file |
| `edit_file` | Find-and-replace in a file |
| `list_dir` | List a directory |
| `run_command` | Run a shell command |
| `create_dir` | Create a directory |
| `file_exists` | Check if a file exists |
| `delete_file` | Delete a file |
| `read_files` | Batch read multiple files |

Dangerous commands (`rm`, `sudo`, `dd`, etc.) default to **no**.

## Commands

| Command | Description |
|---------|-------------|
| `/help` | Show help |
| `/quit` | Exit |
| `/new`  | Start a fresh session |

## Install

```bash
cargo install --path .
attacca
```
