import type { Transport, TransportFactory, Message } from "../ws-client.js";

type MessageHandler = (data: string) => void;
type CloseHandler = () => void;
type ErrorHandler = (err: Error) => void;

/**
 * A fake transport for testing WsClient without real WebSockets.
 * Captures sent messages and allows simulating received messages.
 */
export class FakeTransport implements Transport {
  sent: string[] = [];
  private messageHandlers: MessageHandler[] = [];
  private closeHandlers: CloseHandler[] = [];
  closed = false;

  send(data: string): void {
    this.sent.push(data);
  }

  on(event: "message", handler: MessageHandler): void;
  on(event: "close", handler: CloseHandler): void;
  on(event: "error", handler: ErrorHandler): void;
  on(
    event: string,
    handler: MessageHandler | CloseHandler | ErrorHandler,
  ): void {
    if (event === "message") {
      this.messageHandlers.push(handler as MessageHandler);
    } else if (event === "close") {
      this.closeHandlers.push(handler as CloseHandler);
    }
  }

  close(): void {
    this.closed = true;
    this.closeHandlers.forEach((h) => h());
  }

  /** Simulate receiving a message from the server. */
  receive(msg: Message): void {
    const data = JSON.stringify(msg);
    this.messageHandlers.forEach((h) => h(data));
  }

  /** Get the last sent message as parsed JSON. */
  lastSent(): Message | undefined {
    if (this.sent.length === 0) return undefined;
    return JSON.parse(this.sent[this.sent.length - 1]);
  }
}

/**
 * Creates a TransportFactory that auto-responds to register messages.
 * Returns the FakeTransport for further interaction.
 */
export function createAutoRegisterFactory(
  agentId = "test-agent-1",
): { factory: TransportFactory; getTransport: () => FakeTransport } {
  let transport: FakeTransport;

  const factory: TransportFactory = async () => {
    transport = new FakeTransport();

    // After handlers are registered, auto-respond to register
    queueMicrotask(() => {
      const checkForRegister = () => {
        const last = transport.lastSent();
        if (last && last.type === "register") {
          transport.receive({
            id: last.id,
            type: "register_ack",
            from: "orchestrator",
            to: agentId,
            payload: { agentId, peers: [] },
          });
        } else {
          setTimeout(checkForRegister, 5);
        }
      };
      checkForRegister();
    });

    return transport;
  };

  return { factory, getTransport: () => transport! };
}
