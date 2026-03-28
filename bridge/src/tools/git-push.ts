import { WsClient } from "../ws-client.js";

export async function handleGitPush(
  client: WsClient,
  args: { remote?: string; branch?: string },
): Promise<string> {
  const response = await client.sendRequest("git_push", {
    remote: args.remote ?? "origin",
    branch: args.branch ?? "",
  });

  if (response.type === "error") {
    throw new Error(
      (response.payload.message as string) ?? "Permission denied",
    );
  }

  return (response.payload.output as string) ?? "Push completed";
}
