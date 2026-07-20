import * as assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { MooseDevPush } from "../../../.opencode/plugins/moosedev-push";

const PROJECT_GRAPH = "https://moosedev.dev/kg/project";

async function exerciseCheckpoint(servedDataDir: (actual: string) => string) {
  const root = mkdtempSync(join(tmpdir(), "moosedev-opencode-identity-"));
  const dataDir = join(root, ".moosedev");
  mkdirSync(dataDir);
  const actualDataDir = realpathSync.native(dataDir);
  const captures: unknown[] = [];

  const server = createServer((request, response) => {
    if (request.method === "GET" && request.url === "/api/v1/health") {
      const body = JSON.stringify({
        status: "ok",
        project_graph: PROJECT_GRAPH,
        data_dir: servedDataDir(actualDataDir),
      });
      response.writeHead(200, { "Content-Type": "application/json" });
      response.end(body);
      return;
    }
    if (request.method === "POST" && request.url === "/api/v1/capture") {
      let body = "";
      request.setEncoding("utf8");
      request.on("data", (chunk) => (body += chunk));
      request.on("end", () => {
        captures.push(JSON.parse(body));
        response.writeHead(200, { "Content-Type": "application/json" });
        response.end("{}");
      });
      return;
    }
    response.writeHead(404);
    response.end();
  });

  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  assert.ok(address && typeof address !== "string");
  writeFileSync(join(dataDir, "http.addr"), `127.0.0.1:${address.port}\n`);

  const plugin = await MooseDevPush({ directory: root });
  await plugin.event({
    event: { type: "file.edited", properties: { file: join(root, "src/lib.rs") } },
  });
  await plugin.event({ event: { type: "session.idle" } });

  await new Promise<void>((resolve) => server.close(() => resolve()));
  rmSync(root, { recursive: true, force: true });
  return captures;
}

test("OpenCode checkpoints require matching canonical health identity", async (t) => {
  await t.test("matching identity permits capture", async () => {
    const captures = await exerciseCheckpoint((actual) => actual);
    assert.deepEqual(captures, [{ host: "opencode", files: ["src/lib.rs"] }]);
  });

  await t.test("mismatched identity suppresses capture", async () => {
    const originalWarn = console.warn;
    console.warn = () => undefined;
    try {
      const captures = await exerciseCheckpoint((actual) => `${actual}-other`);
      assert.deepEqual(captures, []);
    } finally {
      console.warn = originalWarn;
    }
  });
});
