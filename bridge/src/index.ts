import { WsClient } from "./ws-client.js";
import { startMcpServer } from "./mcp-server.js";
import { TaskQueue } from "./task-queue.js";

const orchestratorUrl =
  process.env.ORCHESTRATOR_URL ?? "ws://localhost:9800";
const agentName = process.env.AGENT_NAME ?? "unnamed-agent";
const agentRole = process.env.AGENT_ROLE ?? "code-agent";
const agentMode = process.env.AGENT_MODE ?? "oneshot";

async function main() {
  console.error(`[bridge] Connecting to orchestrator at ${orchestratorUrl}`);

  const client = new WsClient(orchestratorUrl, agentName, agentRole);
  await client.connect();

  if (agentMode === "long-running") {
    // Long-running mode: run as a persistent background process.
    // Serve the task queue HTTP endpoint for the entrypoint to poll.
    // Do NOT start the MCP stdio server (no stdin available).
    const taskQueue = new TaskQueue();
    taskQueue.startServer(9801);

    client.onPush((msg) => {
      if (msg.type === "send_task") {
        const prompt = msg.payload.prompt as string;
        if (prompt) {
          console.error(`[bridge] Received task: ${prompt.slice(0, 80)}...`);
          taskQueue.push(prompt);
        }
      }
    });

    console.error("[bridge] Task queue ready (long-running mode)");
    // Keep process alive
  } else {
    // One-shot mode: run as MCP server on stdio (launched by Claude Code).
    console.error("[bridge] Starting MCP server on stdio");
    await startMcpServer(client);
  }
}

main().catch((err) => {
  console.error("[bridge] Fatal error:", err);
  process.exit(1);
});
