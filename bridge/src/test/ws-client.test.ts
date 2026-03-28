import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { WsClient } from "../ws-client.js";
import { FakeTransport, createAutoRegisterFactory } from "./fake-transport.js";

describe("WsClient", () => {
  it("sends register on connect and stores agent ID", async () => {
    const { factory, getTransport } = createAutoRegisterFactory("agent-42");
    const client = new WsClient("ws://fake", "my-agent", "code-agent", factory);

    await client.connect();

    const transport = getTransport();
    const registerMsg = JSON.parse(transport.sent[0]);
    assert.equal(registerMsg.type, "register");
    assert.equal(registerMsg.payload.name, "my-agent");
    assert.equal(registerMsg.payload.role, "code-agent");
    assert.equal(registerMsg.from, "pending");

    client.close();
  });

  it("sendRequest sends message and resolves on response", async () => {
    const { factory, getTransport } = createAutoRegisterFactory();
    const client = new WsClient("ws://fake", "test", "code-agent", factory);
    await client.connect();

    const transport = getTransport();

    const promise = client.sendRequest("user_prompt", {
      question: "What color?",
    });

    // Wait for the message to be sent
    await new Promise((r) => setTimeout(r, 10));

    // Find the user_prompt message (skip the register message)
    const promptMsg = JSON.parse(
      transport.sent.find((s) => JSON.parse(s).type === "user_prompt")!,
    );
    assert.equal(promptMsg.type, "user_prompt");
    assert.equal(promptMsg.payload.question, "What color?");

    // Simulate response
    transport.receive({
      id: promptMsg.id,
      type: "user_prompt_response",
      from: "orchestrator",
      payload: { answer: "blue" },
    });

    const response = await promise;
    assert.equal(response.payload.answer, "blue");

    client.close();
  });

  it("rejects pending requests when transport closes", async () => {
    const { factory, getTransport } = createAutoRegisterFactory();
    const client = new WsClient("ws://fake", "test", "code-agent", factory);
    await client.connect();

    const transport = getTransport();

    const promise = client.sendRequest("user_prompt", {
      question: "test",
    });

    await new Promise((r) => setTimeout(r, 10));

    transport.close();

    await assert.rejects(promise, { message: "WebSocket connection closed" });
  });

  it("throws when sending before connect", async () => {
    const client = new WsClient("ws://fake", "test", "code-agent");

    await assert.rejects(
      client.sendRequest("user_prompt", { question: "test" }),
      { message: "Not connected to orchestrator" },
    );
  });
});
