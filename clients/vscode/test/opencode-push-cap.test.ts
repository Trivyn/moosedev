import * as assert from "node:assert/strict";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { MooseDevPush } from "../../../.opencode/plugins/moosedev-push";

const NOTICE = "[truncated by hook cap; call get_entity_dossier for the full dossier]";

// A stand-in for `moosedev --connect`: answers MCP initialize and one
// evaluate_policy call with a dossier longer than the plugin's block cap, and
// logs the tool arguments it received.
const FAKE_BIN = `#!/usr/bin/env node
const { appendFileSync } = require("node:fs")
const { createInterface } = require("node:readline")
const dossier = Array.from({ length: 200 }, (_, i) => "- line " + String(i).padStart(4, "0") + " " + "x".repeat(40)).join("\\n")
createInterface({ input: process.stdin }).on("line", (line) => {
  const message = JSON.parse(line)
  if (message.id === undefined) return
  let result = {}
  if (message.method === "tools/call") {
    appendFileSync(process.env.MOOSEDEV_FAKE_LOG, JSON.stringify(message.params.arguments) + "\\n")
    const verdict = { decision: "inject", dossier_markdown: dossier, entities: ["urn:e1"], records: [] }
    result = { content: [{ type: "text", text: JSON.stringify(verdict) }] }
  }
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: message.id, result }) + "\\n")
})
`;

test("OpenCode push passes its remaining budget and cuts on a line boundary", async () => {
  const root = mkdtempSync(join(tmpdir(), "moosedev-opencode-push-cap-"));
  const bin = join(root, "fake-moosedev");
  const log = join(root, "calls.jsonl");
  writeFileSync(bin, FAKE_BIN);
  chmodSync(bin, 0o755);
  const saved = { bin: process.env.MOOSEDEV_BIN, log: process.env.MOOSEDEV_FAKE_LOG };
  process.env.MOOSEDEV_BIN = bin;
  process.env.MOOSEDEV_FAKE_LOG = log;
  try {
    const plugin = await MooseDevPush({ directory: root });
    await plugin["tool.execute.after"](
      { tool: "read", sessionID: "s1", callID: "c1", args: { filePath: join(root, "src/lib.rs") } },
      { title: "", output: "", metadata: {} },
    );
    const output = { system: [] as string[] };
    await plugin["experimental.chat.system.transform"]({ sessionID: "s1" }, output);

    const calls = readFileSync(log, "utf8").trim().split("\n").map((line) => JSON.parse(line));
    assert.deepEqual(calls[0], {
      host: "opencode",
      event: "entity_touched",
      file: "src/lib.rs",
      max_bytes: 6000,
    });

    const lines = output.system[0].split("\n");
    assert.equal(lines.at(-1), NOTICE);
    const dossierLines = lines.slice(1, -1);
    assert.ok(dossierLines.length > 0);
    for (const line of dossierLines) {
      assert.match(line, /^- line \d{4} x{40}$/, "no dossier line is cut mid-line");
    }
    assert.ok(lines.slice(1).join("\n").length <= 6000, "the block stays within the cap");
  } finally {
    process.env.MOOSEDEV_BIN = saved.bin;
    process.env.MOOSEDEV_FAKE_LOG = saved.log;
    if (saved.bin === undefined) delete process.env.MOOSEDEV_BIN;
    if (saved.log === undefined) delete process.env.MOOSEDEV_FAKE_LOG;
    rmSync(root, { recursive: true, force: true });
  }
});
