import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";
import { WsClient } from "./ws-client.js";
import { handleAskUser } from "./tools/ask-user.js";

export async function startMcpServer(client: WsClient): Promise<void> {
  const server = new McpServer({
    name: "agent-bridge",
    version: "0.1.0",
  });

  server.tool(
    "ask_user",
    "Ask the user a question and get their answer. Use this when you need clarification or approval from the human operator.",
    { question: z.string().describe("The question to ask the user") },
    async ({ question }) => {
      try {
        const answer = await handleAskUser(client, { question });
        return { content: [{ type: "text", text: answer }] };
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        return {
          content: [{ type: "text", text: `Error: ${message}` }],
          isError: true,
        };
      }
    },
  );

  const transport = new StdioServerTransport();
  await server.connect(transport);
}
