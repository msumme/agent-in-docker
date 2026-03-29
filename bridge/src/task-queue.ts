import http from "node:http";

type TaskResolver = (task: string) => void;

/**
 * A simple task queue with an HTTP endpoint for the entrypoint to poll.
 * The orchestrator pushes tasks via WebSocket, the entrypoint fetches
 * them via HTTP GET /next-task.
 */
export class TaskQueue {
  private queue: string[] = [];
  private waiters: TaskResolver[] = [];
  private server: http.Server | null = null;

  push(task: string): void {
    if (this.waiters.length > 0) {
      const waiter = this.waiters.shift()!;
      waiter(task);
    } else {
      this.queue.push(task);
    }
  }

  next(): Promise<string> {
    if (this.queue.length > 0) {
      return Promise.resolve(this.queue.shift()!);
    }
    return new Promise((resolve) => {
      this.waiters.push(resolve);
    });
  }

  startServer(port: number): void {
    this.server = http.createServer((req, res) => {
      if (req.url === "/next-task" && req.method === "GET") {
        let responded = false;

        const timer = setTimeout(() => {
          if (!responded) {
            responded = true;
            res.writeHead(204);
            res.end();
          }
        }, 30_000);

        this.next().then((task) => {
          if (!responded) {
            responded = true;
            clearTimeout(timer);
            res.writeHead(200, { "Content-Type": "text/plain" });
            res.end(task);
          } else {
            // Timeout already fired -- put the task back
            this.queue.unshift(task);
          }
        });
      } else {
        res.writeHead(404);
        res.end();
      }
    });

    this.server.listen(port, "127.0.0.1", () => {
      console.error(
        `[bridge] Task queue listening on http://127.0.0.1:${port}`,
      );
    });
  }

  stop(): void {
    this.server?.close();
  }
}
