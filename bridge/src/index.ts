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

  // In long-running mode, start the task queue HTTP server
  if (agentMode === "long-running") {
    const taskQueue = new TaskQueue();
    taskQueue.startServer(9801);

    // Listen for send_task push messages from orchestrator
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
  }

  console.error("[bridge] Starting MCP server on stdio");
  await startMcpServer(client);
}

main().catch((err) => {
  console.error("[bridge] Fatal error:", err);
  process.exit(1);
});
