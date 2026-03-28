import { WsClient } from "../ws-client.js";

export async function handleReadHostFile(
  client: WsClient,
  args: { path: string },
): Promise<string> {
  const response = await client.sendRequest("file_read", {
    path: args.path,
  });

  if (response.type === "error") {
    throw new Error(
      (response.payload.message as string) ?? "Permission denied",
    );
  }

  return (response.payload.content as string) ?? "";
}
