# Agent Prompt: DOSBox-X MCP Command Check

Use this prompt for an agent that validates the DOSBox-X MCP server tools
against a running DOSBox-X instance.

```text
You are checking the DOSBox-X MCP server integration. Your goal is to verify
that every exposed `dosbox_*` MCP tool works, including tools that change
debugger/emulator state. If the MCP server log is available, also use it to
check request serialization and connection handling.

Prerequisites:
- The DOSBox-X MCP server is running.
- DOSBox-X was built with the MCP debugger control changes.
- DOSBox-X is running and connected to the MCP server.
- For a native local run, the MCP server log may be available at
  `$HOME/.dosbox-x-mcp-server/server.log` or through captured server stderr.
- For a browser build connected over WebSocket, do not assume that the MCP
  server's filesystem or stderr is available.

Important rules:
- Do not assume a timeout is harmless. Treat any timeout as a failure unless
  an available server log clearly explains an expected disconnect/reconnect.
- After every state-changing command, verify that the next command still works.
- When the server log is available and several calls are made in parallel,
  confirm that requests were serialized: one request sent to DOSBox-X, one
  response read, then the next request.
- Use these control log messages as serialization evidence when available:
  `DOSBox-X request send begin`, `DOSBox-X request sent; waiting for response`,
  and `DOSBox-X response received`.
- When the server log is unavailable, do not infer serialization from tool
  completion order. Verify that every parallel call completes, and mark the
  log and serialization checks as SKIPPED.
- At the end, compare your observed tool results with the MCP server log when
  it is available. Report whether it shows an unexpected timeout, disconnect,
  reconnect, panic, aborted request, or dropped request.
- `CancelledNotification` with `AbortError` is a client-side cancellation
  signal. When a log is available, treat the signal as a failure only if it
  appears in the current run and correlates with an aborted or failed tool
  call. Do not use it as DOSBox-X request/response serialization evidence.

Test sequence:

1. Check static tool behavior without relying on DOSBox-X state.
   - Call `dosbox_debug_capabilities({})`.
   - Verify it returns the static debugger command catalog.

2. Check connection health.
   - Call `dosbox_dosbox_ping({})`.
   - Expected result: `PONG`.
   - If it returns `ERR`, stop. Inspect the server log if it is available, and
     otherwise report the exact error returned by the tool.

3. Check live debugger discovery and basic raw execution.
   - Call `dosbox_debug_help({})`.
   - Verify it returns live HELP output from DOSBox-X.
   - Call `dosbox_debug_exec({"command":"CPU"})`.
   - Verify it returns CPU/debugger state text and does not time out.

4. Enter the debugger.
   - Call `dosbox_debug_break({})`.
   - Expected result: success and DOSBox-X enters the built-in debugger.
   - Immediately call `dosbox_debug_exec({"command":"CPU"})`.
   - Verify this still succeeds while DOSBox-X is stopped in the debugger.

5. Check concurrent read-only/debugger-state commands while stopped.
   - In parallel, call:
     - `dosbox_debug_snapshot({})`
     - `dosbox_debug_exec({"command":"CPU"})`
     - `dosbox_debug_breakpoint({"action":"list"})`
   - Expected result: all complete without timeout.
   - `debug_snapshot` should include sections for CPU, PIC, PAGING, EMU MEM,
     and EMU MACHINE.
   - `debug_breakpoint({"action":"list"})` should return breakpoint list
     output or a valid empty-list response.

6. Check breakpoint state changes.
   - Call `dosbox_debug_breakpoint({"action":"set","args":"CS:EIP"})` only if
     the live HELP/output indicates this syntax is accepted; otherwise use a
     known valid BP syntax for the running DOSBox-X build.
   - Call `dosbox_debug_breakpoint({"action":"list"})` and verify the new
     breakpoint is listed.
   - Delete the breakpoint with `dosbox_debug_breakpoint({"action":"delete","args":"<id>"})`
     or `dosbox_debug_breakpoint({"action":"delete","args":"*"})` if cleanup
     by id is not practical.
   - Call `dosbox_debug_breakpoint({"action":"list"})` again and verify cleanup.

7. Check invalid input paths.
   - Call `dosbox_debug_exec({"command":"THIS_COMMAND_SHOULD_NOT_EXIST"})`.
   - Expected result: `ERR` or debugger text indicating the command is not
     recognized.
   - Call `dosbox_debug_run({"mode":"invalid"})`.
   - Expected result: `ERR` with the supported mode list.

8. Check state-changing run commands.
   - Ensure DOSBox-X is in debugger mode with `dosbox_debug_break({})`.
   - Call `dosbox_debug_run({"mode":"vrt"})`.
   - Expected result: success/acceptance, not a post-run state snapshot.
   - Then call `dosbox_dosbox_ping({})`.
   - Expected result: `PONG`.
   - Re-enter debugger with `dosbox_debug_break({})`.
   - Call `dosbox_debug_run({"mode":"run"})`.
   - Expected result: success/acceptance.
   - Then call `dosbox_dosbox_ping({})`.
   - Expected result: `PONG`.
   - If `runwatch` is safe for the current workload, re-enter debugger and call
     `dosbox_debug_run({"mode":"runwatch"})`, then verify `dosbox_dosbox_ping({})`.

9. Check heavy-debug wrappers only if supported by the build.
   - Use `dosbox_debug_help({})` and/or `dosbox_debug_capabilities({})`.
   - If BPM/BPPM/BPLM are supported, test:
     - `dosbox_debug_breakpoint({"action":"mem","args":"<valid args>"})`
     - `dosbox_debug_breakpoint({"action":"pmem","args":"<valid args>"})`
     - `dosbox_debug_breakpoint({"action":"lmem","args":"<valid args>"})`
   - If unsupported, record that they were skipped because the build does not
     expose heavy-debug commands.

10. Server log verification when the log is available.
    - For a local native run, open
      `$HOME/.dosbox-x-mcp-server/server.log`. If the server was launched with
      redirected stderr, you may use that captured stderr output instead.
    - For a browser build connected over WebSocket, use the log only if the
      test environment explicitly provides server filesystem or stderr access.
    - If neither source is available, mark the whole server log verification
      step as SKIPPED and state that server-side serialization was not checked.
    - Confirm that each successful tool call has a matching DOSBox-X request
      and response.
    - For each DOSBox-X request id, verify this ordered pattern:
      `DOSBox-X request send begin` ->
      `DOSBox-X request sent; waiting for response` ->
      `DOSBox-X response received`.
    - Confirm that parallel calls were serialized rather than interleaved as
      multiple pending DOSBox-X requests.
    - Confirm there are no unexpected messages such as:
      - `DOSBox-X request timed out`
      - `DOSBox-X disconnected`
      - `connection error`
      - `control task dropped the request`
      - `Tool execution aborted`
      - Rust panic/backtrace output
    - If a disconnect/reconnect appears, correlate it with the exact tool call
      and decide whether it is expected. Unexpected disconnects are failures.

Final report format:
- List every `dosbox_*` tool tested.
- Mark each as PASS, FAIL, or SKIPPED.
- Include the exact command arguments for state-changing tools.
- Include any timeout or disconnect evidence from the server log when it is
  available.
- State whether the MCP server log agrees with the observed tool results, or
  state that log verification was skipped because the log was unavailable.
- If anything failed, provide the shortest reproducible sequence.
```
