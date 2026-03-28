import { WsClient } from "./ws-client.js";
import { startMcpServer } from "./mcp-server.js";

const orchestratorUrl =
  process.env.ORCHESTRATOR_URL ?? "ws://localhost:9800";
const agentName = process.env.AGENT_NAME ?? "unnamed-agent";
const agentRole = process.env.AGENT_ROLE ?? "code-agent";

async function main() {
  console.error(`[bridge] Connecting to orchestrator at ${orchestratorUrl}`);

  const client = new WsClient(orchestratorUrl, agentName, agentRole);
  await client.connect();

  console.error("[bridge] Starting MCP server on stdio");
  await startMcpServer(client);
}

main().catch((err) => {
  console.error("[bridge] Fatal error:", err);
  process.exit(1);
});
