import { WsClient } from "../ws-client.js";

export const askUserTool = {
  name: "ask_user",
  description:
    "Ask the user a question and get their answer. Use this when you need clarification or approval from the human operator.",
  inputSchema: {
    type: "object" as const,
    properties: {
      question: {
        type: "string",
        description: "The question to ask the user",
      },
    },
    required: ["question"],
  },
};

export async function handleAskUser(
  client: WsClient,
  args: { question: string },
): Promise<string> {
  const response = await client.sendRequest("user_prompt", {
    question: args.question,
  });
  const answer = response.payload.answer as string;
  return answer;
}
