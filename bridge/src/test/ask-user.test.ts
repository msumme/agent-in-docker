import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { WsClient } from "../ws-client.js";
import { createAutoRegisterFactory } from "./fake-transport.js";
import { handleAskUser } from "../tools/ask-user.js";

describe("handleAskUser", () => {
  it("sends user_prompt and returns the answer", async () => {
    const { factory, getTransport } = createAutoRegisterFactory();
    const client = new WsClient("ws://fake", "test", "code-agent", factory);
    await client.connect();

    const transport = getTransport();

    const promise = handleAskUser(client, { question: "Favorite food?" });

    // Wait for message to be sent
    await new Promise((r) => setTimeout(r, 10));

    const promptMsg = JSON.parse(
      transport.sent.find((s) => JSON.parse(s).type === "user_prompt")!,
    );

    transport.receive({
      id: promptMsg.id,
      type: "user_prompt_response",
      from: "orchestrator",
      payload: { answer: "pizza" },
    });

    const answer = await promise;
    assert.equal(answer, "pizza");

    client.close();
  });
});
