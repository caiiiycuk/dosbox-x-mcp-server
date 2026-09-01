# DOSBox-X MCP Server
[![CI](https://github.com/caiiiycuk/dosbox-x-mcp-server/actions/workflows/ci.yml/badge.svg)](https://github.com/caiiiycuk/dosbox-x-mcp-server/actions/workflows/ci.yml)

Minimal MCP server for controlling the DOSBox-X built-in debugger from an
agent through MCP tools.

Architecture and tool details are in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Build

```sh
cargo build --release
```

## OpenCode

Download `dosbox-x-mcp-server` from the
[GitHub Releases](https://github.com/caiiiycuk/dosbox-x-mcp-server/releases)
section, then add it to `opencode.json`.

Add this to `opencode.json` in your project root or to your global OpenCode
config:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
      "dosbox": {
        "type": "local",
        "command": [
          "<path>/dosbox-x-mcp-server"
        ]
      }
    }
  }
}
```

For development, you can run through Cargo instead:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "servers": {
      "dosbox": {
        "type": "local",
        "command": [
          "cargo",
          "run",
          "--quiet",
          "--manifest-path",
          "<path>/dosbox-x-mcp-server/Cargo.toml"
        ]
      }
    }
  }
}
```

OpenCode starts the MCP server over stdio. The MCP server listens for DOSBox-X
on `127.0.0.1:58991` and writes logs to:

```text
$HOME/.dosbox-x-mcp-server/server.log
```

## Start DOSBox-X

Start OpenCode first so it launches the MCP server:

```sh
opencode
```

Then start a DOSBox-X build that includes the MCP debugger control changes:

```sh
./dosbox-x
```

DOSBox-X connects back to the MCP server automatically. In OpenCode, use tools
such as `dosbox_dosbox_ping`, `dosbox_debug_break`,
`dosbox_debug_exec`, `dosbox_debug_snapshot`, and
`dosbox_debug_run`.

## Verify integration

You can verify integration by pasting [prompt](docs/AGENT_COMMAND_CHECK_PROMPT.md) for agent
