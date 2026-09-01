# Agent Prompt: DOSBox-X MCP Command Check

Use this prompt for an agent that validates the DOSBox-X MCP server tools
against a running DOSBox-X instance.

```text
You are checking the DOSBox-X MCP server integration. Your goal is to verify
that every exposed `dosbox_*` MCP tool works, including tools that change
debugger/emulator state, and that the MCP server log confirms healthy request
serialization and connection handling.

Prerequisites:
- The DOSBox-X MCP server is running.
- DOSBox-X was built with the MCP debugger control changes.
- DOSBox-X is running and connected to the MCP server.
- You have access to the MCP server log file:
  `$HOME/.dosbox-x-mcp-server/server.log`.
- The server also mirrors the same log output to stderr.

Important rules:
- Do not assume a timeout is harmless. Treat any timeout as a failure unless
  the server log clearly explains an expected disconnect/reconnect.
- After every state-changing command, verify that the next command still works.
- When several calls are made in parallel, confirm from the server log that
  requests were serialized: one request sent to DOSBox-X, one response read,
  then the next request.
- Use these control log messages as serialization evidence:
  `DOSBox-X request send begin`, `DOSBox-X request sent; waiting for response`,
  and `DOSBox-X response received`.
- At the end, compare your observed tool results with the MCP server log and
  explicitly report whether the log shows unexpected timeout, disconnect,
  reconnect, panic, aborted request, or dropped request messages.
- `CancelledNotification` with `AbortError` is a client-side cancellation
  signal. Treat it as a failure only if it appears in the current run's log and
  correlates with an aborted/failed tool call; do not use it as DOSBox-X
  request/response serialization evidence.

Test sequence:

1. Check static tool behavior without relying on DOSBox-X state.
   - Call `dosbox_debug_capabilities({})`.
   - Verify it returns the static debugger command catalog.

2. Check connection health.
   - Call `dosbox_dosbox_ping({})`.
   - Expected result: `PONG`.
   - If it returns `ERR`, stop and inspect the server log before continuing.

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

10. Server log verification.
    - Open `$HOME/.dosbox-x-mcp-server/server.log`.
    - If the server was launched with redirected stderr, you may also compare
      against that captured stderr output; it should contain the same log
      stream.
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
- Include any timeout/disconnect evidence from the server log.
- State whether the MCP server log agrees with the observed tool results.
- If anything failed, provide the shortest reproducible sequence.
```
