// Splitlane status bridge for Pi - INSTALLED AND REMOVED AUTOMATICALLY by
// splitlane-shim around each `pi` session started inside a Splitlane terminal.
// Safe to delete; do not edit (changes are overwritten).
//
// Reports lifecycle to the Splitlane sidebar by connecting to Splitlane's IPC
// endpoint (SPLITLANE_SOCKET_PATH) and writing a single JSON-RPC frame, then
// closing - NO `splitlane-ai-hook` subprocess. On Windows, repeatedly spawning
// `splitlane-ai-hook.exe` from the agent fails to start (0xC0000142) and pops
// error dialogs; a direct socket write avoids all of that. Inert outside a
// Splitlane PTY (the env vars are absent there).

import net from "node:net";

export default function (pi) {
  const send = (method, params) => {
    const sock = process.env["SPLITLANE_SOCKET_PATH"];
    const wsId = process.env["SPLITLANE_WORKSPACE_ID"];
    if (!sock || !wsId) return;
    try {
      const p = {
        workspace_id: Number(wsId),
        tool: "pi",
        pid: Number(process.env["SPLITLANE_AI_PID"] || process.pid),
        ...(params ?? {}),
      };
      const sid = process.env["SPLITLANE_SURFACE_ID"];
      if (sid) p.surface_id = Number(sid);
      const frame =
        JSON.stringify({ jsonrpc: "2.0", method, params: p, id: 1 }) + "\n";
      const conn = net.connect(sock);
      conn.on("error", () => {});
      conn.on("connect", () => {
        conn.end(frame);
      });
    } catch {
      // Status reporting must never break the session.
    }
  };
  pi.on("agent_start", () => send("ai.prompt_submit", { hook_payload: {} }));
  pi.on("agent_end", () => send("ai.stop", { hook_payload: {} }));
  pi.on("tool_execution_start", () => send("ai.tool_use", { hook_payload: {} }));
  pi.on("tool_execution_end", () => send("ai.tool_use", { hook_payload: {} }));
}
