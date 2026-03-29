import { WsClient } from "./ws-client.js";
import { startMcpServer, startHttpMcpServer } from "./mcp-server.js";
import { TaskQueue } from "./task-queue.js";

const orchestratorUrl =
  process.env.ORCHESTRATOR_URL ?? "ws://localhost:9800";
const agentName = process.env.AGENT_NAME ?? "unnamed-agent";
const agentRole = process.env.AGENT_ROLE ?? "code-agent";
const agentMode = process.env.AGENT_MODE ?? "oneshot";
const mcpPort = parseInt(process.env.MCP_PORT ?? "0", 10);

async function main() {
  console.error(`[bridge] Connecting to orchestrator at ${orchestratorUrl}`);

  const client = new WsClient(orchestratorUrl, agentName, agentRole);
  await client.connect();

  if (agentMode === "host") {
    // Host-side shared mode: single bridge process serving all agents via HTTP MCP
    const port = mcpPort || 9801;
    await startHttpMcpServer(client, port);
    console.error(`[bridge] Running in host mode on port ${port}`);
    // Keep process alive
    return;
  }

  if (agentMode === "long-running") {
    // Long-running container mode: task queue for orchestrator-dispatched tasks
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
    return;
  }

  // In-container oneshot mode: MCP on stdio (launched by Claude Code as subprocess)
  console.error("[bridge] Starting MCP server on stdio");
  await startMcpServer(client);
}

main().catch((err) => {
  console.error("[bridge] Fatal error:", err);
  process.exit(1);
});
