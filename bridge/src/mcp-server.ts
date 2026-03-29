import http from "node:http";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";
import { z } from "zod";
import { WsClient } from "./ws-client.js";
import { handleAskUser } from "./tools/ask-user.js";
import { handleReadHostFile } from "./tools/read-host-file.js";
import { handleGitPush } from "./tools/git-push.js";

function registerTools(server: McpServer, client: WsClient): void {
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

  server.tool(
    "read_host_file",
    "Read a file from the host machine (outside the container workspace). Only allowed paths are accessible, and the human operator must approve each read.",
    { path: z.string().describe("Absolute path to the file on the host") },
    async ({ path }) => {
      try {
        const content = await handleReadHostFile(client, { path });
        return { content: [{ type: "text", text: content }] };
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        return {
          content: [{ type: "text", text: `Error: ${message}` }],
          isError: true,
        };
      }
    },
  );

  server.tool(
    "git_push",
    "Push the current branch to a remote using the host's git credentials (SSH keys). The human operator must approve each push.",
    {
      remote: z
        .string()
        .optional()
        .describe("Git remote name (default: origin)"),
      branch: z
        .string()
        .optional()
        .describe("Branch to push (default: current branch)"),
    },
    async ({ remote, branch }) => {
      try {
        const output = await handleGitPush(client, { remote, branch });
        return { content: [{ type: "text", text: output }] };
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        return {
          content: [{ type: "text", text: `Error: ${message}` }],
          isError: true,
        };
      }
    },
  );
}

/** Start MCP server on stdio (for in-container subprocess mode). */
export async function startMcpServer(client: WsClient): Promise<void> {
  const server = new McpServer({ name: "agent-bridge", version: "0.1.0" });
  registerTools(server, client);
  const transport = new StdioServerTransport();
  await server.connect(transport);
}

/** Start MCP server on HTTP (for host-side shared mode). */
export async function startHttpMcpServer(
  client: WsClient,
  port: number,
): Promise<number> {
  const mcpServer = new McpServer({ name: "agent-bridge", version: "0.1.0" });
  registerTools(mcpServer, client);

  const transport = new StreamableHTTPServerTransport({
    sessionIdGenerator: undefined,
  });
  await mcpServer.connect(transport);

  const httpServer = http.createServer(async (req, res) => {
    try {
      await transport.handleRequest(req, res);
    } catch (err) {
      console.error("[bridge] HTTP MCP error:", err);
      if (!res.headersSent) {
        res.writeHead(500);
        res.end("Internal server error");
      }
    }
  });

  return new Promise((resolve) => {
    httpServer.listen(port, "0.0.0.0", () => {
      const addr = httpServer.address();
      const actualPort = typeof addr === "object" && addr ? addr.port : port;
      console.error(
        `[bridge] MCP HTTP server listening on 0.0.0.0:${actualPort}`,
      );
      resolve(actualPort);
    });
  });
}
