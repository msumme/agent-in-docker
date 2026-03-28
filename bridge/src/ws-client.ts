import WebSocket from "ws";
import crypto from "node:crypto";

export interface Message {
  id: string;
  type: string;
  from: string;
  to?: string;
  payload: Record<string, unknown>;
}

type PendingResolver = {
  resolve: (msg: Message) => void;
  reject: (err: Error) => void;
};

/** Minimal interface for a WebSocket-like transport. Injectable for testing. */
export interface Transport {
  send(data: string): void;
  on(event: "message", handler: (data: string) => void): void;
  on(event: "close", handler: () => void): void;
  on(event: "error", handler: (err: Error) => void): void;
  close(): void;
}

/** Factory that creates a Transport given a URL. Injectable for testing. */
export type TransportFactory = (url: string) => Promise<Transport>;

/** Default factory: creates a real WebSocket connection. */
export const websocketTransportFactory: TransportFactory = (
  url: string,
): Promise<Transport> => {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    ws.on("open", () => {
      const transport: Transport = {
        send: (data) => ws.send(data),
        on: ((event: string, handler: (...args: never[]) => void) => {
          if (event === "message") {
            ws.on("message", (data) =>
              (handler as (data: string) => void)(data.toString()),
            );
          } else if (event === "close") {
            ws.on("close", handler as () => void);
          } else if (event === "error") {
            ws.on("error", handler as (err: Error) => void);
          }
        }) as Transport["on"],
        close: () => ws.close(),
      };
      resolve(transport);
    });
    ws.on("error", reject);
  });
};

export class WsClient {
  private transport: Transport | null = null;
  private agentId: string | null = null;
  private pending = new Map<string, PendingResolver>();
  private connected = false;
  private transportFactory: TransportFactory;

  constructor(
    private url: string,
    private name: string,
    private role: string,
    transportFactory?: TransportFactory,
  ) {
    this.transportFactory = transportFactory ?? websocketTransportFactory;
  }

  async connect(): Promise<void> {
    this.transport = await this.transportFactory(this.url);
    this.connected = true;

    this.transport.on("message", (data: string) => {
      let msg: Message;
      try {
        msg = JSON.parse(data);
      } catch {
        console.error("[bridge] Invalid JSON from orchestrator:", data);
        return;
      }
      this.handleMessage(msg);
    });

    this.transport.on("close", () => {
      this.connected = false;
      for (const [, { reject }] of this.pending) {
        reject(new Error("WebSocket connection closed"));
      }
      this.pending.clear();
    });

    this.transport.on("error", (err: Error) => {
      console.error("[bridge] Transport error:", err.message);
    });

    await this.register();
  }

  private async register(): Promise<void> {
    const response = await this.sendRequest("register", {
      name: this.name,
      role: this.role,
    });
    this.agentId = response.payload.agentId as string;
    console.error(
      `[bridge] Registered as ${this.agentId} (${this.name}, ${this.role})`,
    );
  }

  private handleMessage(msg: Message) {
    const resolver = this.pending.get(msg.id);
    if (resolver) {
      this.pending.delete(msg.id);
      resolver.resolve(msg);
    }
  }

  async sendRequest(
    type: string,
    payload: Record<string, unknown>,
  ): Promise<Message> {
    if (!this.transport || !this.connected) {
      throw new Error("Not connected to orchestrator");
    }

    const id = crypto.randomUUID();
    const msg: Message = {
      id,
      type,
      from: this.agentId ?? "pending",
      payload,
    };

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`Request ${type} timed out after 5 minutes`));
      }, 5 * 60 * 1000);

      this.pending.set(id, {
        resolve: (msg) => {
          clearTimeout(timeout);
          resolve(msg);
        },
        reject: (err) => {
          clearTimeout(timeout);
          reject(err);
        },
      });

      this.transport!.send(JSON.stringify(msg));
    });
  }

  close() {
    this.transport?.close();
  }
}
