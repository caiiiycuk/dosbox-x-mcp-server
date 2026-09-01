# Architecture

The MCP server exposes a small set of MCP tools and forwards debugger work to
DOSBox-X over a local text protocol. The primary primitive is
`debug_exec(command)`, which runs an existing DOSBox-X debugger command and
returns captured debugger output.

## Control Connection

The MCP server listens on:

```text
127.0.0.1:58991
```

DOSBox-X acts as a TCP client and connects to that address when the debugger
system is initialized. Start this MCP server before starting DOSBox-X if you
want the connection to be available immediately. DOSBox-X reconnects in the
background if the server is not yet running or if the connection drops.

The process uses MCP stdio for the MCP client connection and writes logs to
stderr and to:

```text
$HOME/.dosbox-x-mcp-server/server.log
```

DOSBox-X connects separately over TCP on `127.0.0.1:58991`.

## Wire Protocol

The wire protocol between this server and DOSBox-X is line-oriented text:

```text
REQ <id> PING
REQ <id> BREAK
REQ <id> EXEC <debugger command>
```

Responses use a framed block:

```text
BEGIN <id> OK
<zero or more output lines>
END <id>
```

Errors use the same framing with `ERR`:

```text
BEGIN <id> ERR
<error text>
END <id>
```

The server serializes calls to DOSBox-X. Only one debugger request is sent
and awaited at a time, even if multiple MCP tool calls arrive concurrently.

## MCP Tools

`dosbox_ping`

Checks the DOSBox-X control connection. Returns `PONG` when DOSBox-X is
connected and responsive. Returns `ERR` text when DOSBox-X is not connected
or the request times out.

`debug_break`

Asks DOSBox-X to enter the built-in debugger.

`debug_exec(command)`

Executes one raw DOSBox-X debugger command, for example `CPU`, `HELP`,
`DOS MCBS`, `PAGING`, or `EMU MEM`.

Call `debug_capabilities` or `debug_help` before using unfamiliar commands.
`debug_exec` is intentionally low-level and uses the live command parser in
DOSBox-X.

`debug_help`

Runs `HELP` in the connected DOSBox-X debugger and returns the live command
list from that build.

`debug_capabilities`

Returns a static catalog of known useful commands and wrappers. This does
not require a DOSBox-X connection.

`debug_snapshot`

Runs this command set in sequence and returns sectioned output:

```text
CPU
PIC
PAGING
EMU MEM
EMU MACHINE
```

`debug_run(mode)`

Wrapper around debugger run commands. Supported modes:

```text
run
runwatch
vrt
```

DOSBox-X acknowledges these resume commands before leaving the debugger loop.
The tool result means the command was accepted; it is not a snapshot of state
after execution resumes.

`debug_breakpoint(action, args)`

Wrapper around common breakpoint commands. Supported actions:

```text
set      -> BP <args>
int      -> BPINT <args>
mem      -> BPM <args>
pmem     -> BPPM <args>
lmem     -> BPLM <args>
delete   -> BPDEL <args>
list     -> BPLIST
```

The `mem`, `pmem`, and `lmem` actions require a DOSBox-X build with
`C_HEAVY_DEBUG`.

## Debugger Command Discovery

Use `debug_capabilities` first for a compact list of common commands and
their intended use.

Use `debug_help` when you need the exact command list supported by the
currently connected DOSBox-X build.

If `debug_exec` returns `ERR` or output saying a command is not recognized,
choose a simpler fallback command or check whether the command depends on a
build option such as `C_HEAVY_DEBUG`.

## Error Behavior

When DOSBox-X is not connected, control tools return:

```text
ERR
DOSBox-X is not connected
```

When DOSBox-X does not answer in time, the request returns:

```text
ERR
DOSBox-X request timed out
```

Timeouts are treated as connection errors internally, so later calls can use
a fresh DOSBox-X reconnect instead of waiting behind a stuck request.

