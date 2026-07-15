import { spawn } from "node:child_process"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { createInterface } from "node:readline"

// MOOSEDev active-agency adapter for opencode (v2.2).
//
// This plugin contains ZERO policy. It observes host events, reports them to
// the MOOSEDev daemon's policy engine (`evaluate_policy` over `moosedev
// --connect`), and enacts the typed verdict it gets back:
//   - PUSH   entity_touched on the session's working set → inject the returned
//            dossier markdown (the same bytes the editor hover shows).
//   - GATE   edit_proposed before Edit/Write/Patch tools → deny blocks the
//            tool call; require_ratification asks via the permission prompt,
//            degrading to a warning note when no prompt fires (spec §4.1:
//            gate where blocking exists, warn-and-inject where only
//            observation exists).
//   - CAPTURE session.idle with fresh edits → one journal line in the
//            daemon's fire telemetry (`POST /api/v1/capture`, debounced).
//            NEVER a graph record: automatic checkpoints are status, not
//            decisions (Lesson 641c1811, AD 007dce15).
// If the daemon is unreachable the plugin fails OPEN (edits proceed, one
// warning) — a memory sidecar must never brick the host.

type PluginInput = {
  directory: string
  worktree?: string
}

type ToolBeforeInput = {
  tool: string
  sessionID: string
  callID: string
}

type ToolBeforeOutput = {
  args: unknown
}

type ToolAfterInput = {
  tool: string
  sessionID: string
  callID: string
  args: unknown
}

type ToolAfterOutput = {
  title: string
  output: string
  metadata: unknown
}

type PermissionInput = {
  id: string
  type: string
  sessionID: string
  callID?: string
  title: string
  metadata: Record<string, unknown>
}

type PermissionOutput = {
  status: "ask" | "deny" | "allow"
}

type HostEvent = {
  type: string
  properties?: Record<string, unknown>
}

type SystemTransformOutput = {
  system: string[]
}

type MessagesTransformOutput = {
  messages: Array<{
    info: unknown
    parts: Array<Record<string, unknown>>
  }>
}

type PolicyVerdict = {
  decision: "allow" | "inject" | "warn" | "gate" | "capture_trigger"
  dossier_markdown?: string
  entities?: string[]
  records?: Array<{ iri: string; kind: string; title: string }>
  disposition?: "deny" | "require_plan" | "require_ratification"
  reason?: string
}

const MAX_WORKING_SET = 8
const MAX_PUSH_FILES = 3
const MAX_BLOCK_CHARS = 6_000
const REQUEST_TIMEOUT_MS = 5_000
const CAPTURE_MIN_INTERVAL_MS = 10 * 60_000
const BLOCK_HEADER = "## Relevant recorded project knowledge (MOOSEDev)"
const GATE_HEADER = "## MOOSEDev gate notices"
const HOST = "opencode"
const PATH_ARG_KEYS = [
  "filePath",
  "filepath",
  "path",
  "absolutePath",
  "relativePath",
  "files",
  "paths",
]
const ANCHOR_ARG_KEYS = ["oldString", "old_string", "oldText"]

export async function MooseDevPush(input: PluginInput) {
  const root = input.worktree || input.directory || process.cwd()
  const workingSetBySession = new Map<string, string[]>()
  const gateVerdictsByCall = new Map<string, PolicyVerdict>()
  const gateNotices: string[] = []
  const editedFiles = new Set<string>()
  let lastCaptureAt = 0
  const warned = new Set<string>()

  function warnOnce(key: string, message: string) {
    if (warned.has(key)) return
    warned.add(key)
    console.warn(`[moosedev-push] ${message}`)
  }

  function workingSet(sessionID: string) {
    let set = workingSetBySession.get(sessionID)
    if (!set) {
      set = []
      workingSetBySession.set(sessionID, set)
    }
    return set
  }

  function addWorkingPaths(sessionID: string, paths: string[]) {
    const set = workingSet(sessionID)
    const defaultSet = sessionID === "default" ? set : workingSet("default")
    for (const raw of paths) {
      const normalized = normalizeProjectPath(raw, root)
      if (!normalized) continue
      pushRecent(set, normalized)
      if (defaultSet !== set) pushRecent(defaultSet, normalized)
    }
  }

  async function evaluatePolicy(
    args: Record<string, unknown>,
  ): Promise<PolicyVerdict | undefined> {
    const text = await callTool("evaluate_policy", { host: HOST, ...args }, warnOnce)
    if (!text) return undefined
    try {
      return JSON.parse(text) as PolicyVerdict
    } catch {
      warnOnce("verdict-json", "evaluate_policy returned non-JSON; ignoring this verdict.")
      return undefined
    }
  }

  /// Resolve (and cache) the gate verdict for one tool call.
  async function gateVerdict(
    callID: string,
    file: string,
    anchor: string | undefined,
  ): Promise<PolicyVerdict | undefined> {
    const cached = gateVerdictsByCall.get(callID)
    if (cached) return cached
    const verdict = await evaluatePolicy({
      event: "edit_proposed",
      file,
      ...(anchor ? { anchor } : {}),
    })
    if (verdict) gateVerdictsByCall.set(callID, verdict)
    return verdict
  }

  function queueGateNotice(verdict: PolicyVerdict) {
    const note = `- ${verdict.reason || "a recorded constraint governs the edited code"}`
    if (!gateNotices.includes(note)) gateNotices.push(note)
    if (gateNotices.length > 4) gateNotices.shift()
  }

  /// Entity-exact push: ask the policy engine about the session's most recent
  /// files and inject the dossiers it returns — resolver-backed, not lexical.
  async function knowledgeBlock(sessionID: string): Promise<string | undefined> {
    const paths = [
      ...new Set([...workingSet(sessionID), ...workingSet("default")]),
    ].slice(0, MAX_PUSH_FILES)

    const sections: string[] = []
    const seenEntities = new Set<string>()
    for (const file of paths) {
      const verdict = await evaluatePolicy({ event: "entity_touched", file })
      if (!verdict || verdict.decision !== "inject" || !verdict.dossier_markdown) continue
      const entities = verdict.entities || []
      if (entities.length > 0 && entities.every((e) => seenEntities.has(e))) continue
      for (const entity of entities) seenEntities.add(entity)
      sections.push(verdict.dossier_markdown)
      if (sections.join("\n").length > MAX_BLOCK_CHARS) break
    }

    const parts: string[] = []
    if (gateNotices.length > 0) {
      parts.push(`${GATE_HEADER}\n${gateNotices.join("\n")}`)
    }
    if (sections.length > 0) {
      parts.push(`${BLOCK_HEADER}\n${capBlock(sections.join("\n\n"), MAX_BLOCK_CHARS)}`)
    }
    if (parts.length === 0) return undefined
    return parts.join("\n\n")
  }

  return {
    // GATE — evaluate before the edit executes; throwing blocks the tool call.
    "tool.execute.before": async (toolInput: ToolBeforeInput, output: ToolBeforeOutput) => {
      if (!isEditTool(toolInput.tool)) return
      const paths = new Set<string>()
      collectPathArgs(output.args, paths)
      const file = [...paths]
        .map((p) => normalizeProjectPath(p, root))
        .find((p): p is string => Boolean(p))
      if (!file) return
      const anchor = firstStringArg(output.args, ANCHOR_ARG_KEYS)

      const verdict = await gateVerdict(toolInput.callID, file, anchor)
      if (!verdict || verdict.decision !== "gate") return
      if (verdict.disposition === "deny") {
        throw new Error(`MOOSEDev gate (deny): ${verdict.reason || "recorded constraint violation"}`)
      }
      // require_ratification: the permission prompt (below) asks when it fires;
      // otherwise degrade to a warning note injected next turn.
      queueGateNotice(verdict)
    },

    // GATE — typed permission surface, when the host prompts for this call.
    "permission.ask": async (permission: PermissionInput, output: PermissionOutput) => {
      let verdict = permission.callID
        ? gateVerdictsByCall.get(permission.callID)
        : undefined
      if (!verdict) {
        const paths = new Set<string>()
        collectPathArgs(permission.metadata, paths)
        const file = [...paths]
          .map((p) => normalizeProjectPath(p, root))
          .find((p): p is string => Boolean(p))
        if (!file) return
        const anchor = firstStringArg(permission.metadata, ANCHOR_ARG_KEYS)
        verdict = permission.callID
          ? await gateVerdict(permission.callID, file, anchor)
          : await evaluatePolicy({ event: "edit_proposed", file, ...(anchor ? { anchor } : {}) })
      }
      if (!verdict || verdict.decision !== "gate") return
      output.status = verdict.disposition === "deny" ? "deny" : "ask"
    },

    // PUSH input — track the session's working set from observed tool activity.
    "tool.execute.after": async (toolInput: ToolAfterInput, output: ToolAfterOutput) => {
      const paths = extractToolPaths(toolInput.tool, toolInput.args, output.output)
      if (paths.length > 0) addWorkingPaths(toolInput.sessionID, paths)
    },

    // CAPTURE — accumulate edited files; journal at idle checkpoints, debounced.
    event: async ({ event }: { event: HostEvent }) => {
      if (event.type === "file.edited") {
        const file = normalizeProjectPath(String(event.properties?.file || ""), root)
        if (file && !isMooseDevStatePath(file)) editedFiles.add(file)
        return
      }
      if (event.type !== "session.idle" || editedFiles.size === 0) return
      const now = Date.now()
      if (now - lastCaptureAt < CAPTURE_MIN_INTERVAL_MS) return
      const files = [...editedFiles]
      const journaled = await journalCheckpoint(root, files, warnOnce)
      // Only a confirmed journal write clears the file set — failures retain
      // it so the next idle event can retry.
      if (!journaled) return
      lastCaptureAt = now
      for (const file of files) editedFiles.delete(file)
    },

    "experimental.chat.system.transform": async (
      transformInput: { sessionID?: string },
      output: SystemTransformOutput,
    ) => {
      const sessionID = transformInput.sessionID || "default"
      const block = await knowledgeBlock(sessionID)
      if (!block) return

      if (output.system.length === 0) output.system.push(block)
      else output.system[0] = `${block}\n\n${output.system[0]}`
    },

    "experimental.chat.messages.transform": async (_input: unknown, output: MessagesTransformOutput) => {
      const block = await knowledgeBlock("default")
      if (!block || output.messages.length === 0) return

      const first = output.messages[0]
      first.parts.unshift({
        id: "moosedev-push-context",
        sessionID: "default",
        messageID: "moosedev-push-context",
        type: "text",
        text: block,
        synthetic: true,
      })
    },
  }
}

function isEditTool(tool: string): boolean {
  const lower = tool.toLowerCase()
  return lower.includes("edit") || lower.includes("write") || lower.includes("patch")
}

function firstStringArg(value: unknown, keys: string[]): string | undefined {
  for (const key of keys) {
    const candidate = getObjectValue(value, key)
    if (typeof candidate === "string" && candidate.length > 0) return candidate
  }
  return undefined
}

function extractToolPaths(tool: string, args: unknown, output: string): string[] {
  const lowerTool = tool.toLowerCase()
  const paths = new Set<string>()

  collectPathArgs(args, paths)

  if (lowerTool.includes("patch")) {
    for (const path of extractPatchPaths(String(getObjectValue(args, "patchText") || output || ""))) {
      paths.add(path)
    }
  }

  if (lowerTool.includes("glob") || lowerTool.includes("grep") || lowerTool.includes("list")) {
    for (const path of extractOutputPaths(output)) paths.add(path)
  }

  return [...paths]
}

function collectPathArgs(value: unknown, paths: Set<string>) {
  if (!value || typeof value !== "object") return
  if (Array.isArray(value)) {
    for (const item of value) collectPathArgs(item, paths)
    return
  }

  const obj = value as Record<string, unknown>
  for (const key of PATH_ARG_KEYS) {
    collectPathValue(obj[key], paths)
  }
}

function collectPathValue(value: unknown, paths: Set<string>) {
  if (typeof value === "string") {
    paths.add(value)
    return
  }
  if (Array.isArray(value)) {
    for (const item of value) collectPathValue(item, paths)
  }
}

function getObjectValue(value: unknown, key: string): unknown {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined
  return (value as Record<string, unknown>)[key]
}

function extractPatchPaths(text: string): string[] {
  const paths: string[] = []
  const re = /^\*\*\* (?:Add|Update|Delete) File: (.+)$|^\*\*\* Move to: (.+)$/gm
  let match: RegExpExecArray | null
  while ((match = re.exec(text))) {
    paths.push((match[1] || match[2] || "").trim())
  }
  return paths
}

function extractOutputPaths(output: string): string[] {
  const paths: string[] = []
  for (const line of output.split(/\r?\n/)) {
    const trimmed = line.trim()
    if (!trimmed || trimmed.includes("://")) continue
    const candidate = trimmed.split(/:\d+(?::\d+)?/)[0] || trimmed
    if (candidate.includes("/") || /\.[A-Za-z0-9]{1,12}$/.test(candidate)) {
      paths.push(candidate)
    }
  }
  return paths
}

function normalizeProjectPath(raw: string, root: string): string | undefined {
  const trimmed = raw.trim().replace(/^["']|["']$/g, "")
  if (!trimmed || trimmed.startsWith("-")) return undefined

  let path = trimmed.replace(/\\/g, "/")
  const normalizedRoot = root.replace(/\\/g, "/").replace(/\/+$/, "")
  if (path.startsWith(`${normalizedRoot}/`)) path = path.slice(normalizedRoot.length + 1)
  if (path.startsWith("./")) path = path.slice(2)
  if (path.startsWith("../") || path === "..") return undefined
  if (path.startsWith("/")) return undefined
  if (path.includes("\0")) return undefined
  return path.replace(/\/+/g, "/")
}

function isMooseDevStatePath(path: string): boolean {
  return path === ".moosedev" || path.startsWith(".moosedev/")
}

function pushRecent(set: string[], value: string) {
  const existing = set.indexOf(value)
  if (existing >= 0) set.splice(existing, 1)
  set.unshift(value)
  if (set.length > MAX_WORKING_SET) set.length = MAX_WORKING_SET
}

/// Journal one automatic session checkpoint to the daemon's fire telemetry
/// (`POST /api/v1/capture`). Never writes the graph — deliberate capture is
/// the only record-minting path. Returns true only on a confirmed 2xx.
async function journalCheckpoint(
  root: string,
  files: string[],
  warnOnce: (key: string, message: string) => void,
): Promise<boolean> {
  const dataDir = process.env.MOOSEDEV_DATA_DIR || ".moosedev"
  let addr: string
  try {
    addr = readFileSync(join(root, dataDir, "http.addr"), "utf8").trim()
  } catch {
    warnOnce("journal-addr", "no MOOSEDev http.addr; session checkpoint not journaled")
    return false
  }
  if (!addr) return false
  try {
    const response = await fetch(`http://${addr}/api/v1/capture`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ host: HOST, files }),
      signal: AbortSignal.timeout(10_000),
    })
    if (!response.ok) {
      warnOnce("journal-http", `session checkpoint journal failed: HTTP ${response.status}`)
      return false
    }
    return true
  } catch (error) {
    warnOnce("journal-http", `session checkpoint journal failed: ${error}`)
    return false
  }
}

/// Spawn `moosedev --connect`, call one MCP tool, return its text content.
/// Requires a running `--serve` backend (MOOSEDEV_NO_AUTOSPAWN).
async function callTool(
  name: string,
  args: Record<string, unknown>,
  warnOnce: (key: string, message: string) => void,
): Promise<string | undefined> {
  // Default to the conventional per-project store, so `moosedev init --opencode`
  // works with no extra env config when opencode runs from the project root.
  const dataDir = process.env.MOOSEDEV_DATA_DIR || ".moosedev"
  const bin = process.env.MOOSEDEV_BIN || "moosedev"
  const child = spawn(bin, ["--connect"], {
    env: {
      ...process.env,
      MOOSEDEV_DATA_DIR: dataDir,
      MOOSEDEV_NO_AUTOSPAWN: "1",
    },
    stdio: ["pipe", "pipe", "pipe"],
  })

  let stderr = ""
  const pending = new Map<number, (value: JsonRpcResponse) => void>()
  child.on("error", (error) => {
    for (const resolve of pending.values()) {
      resolve({ error: { message: error.message } })
    }
    pending.clear()
  })
  child.stderr.setEncoding("utf8")
  child.stderr.on("data", (chunk) => {
    stderr += chunk
  })

  const lines = createInterface({ input: child.stdout })
  lines.on("line", (line) => {
    try {
      const message = JSON.parse(line) as JsonRpcResponse
      if (typeof message.id === "number") pending.get(message.id)?.(message)
    } catch {
      warnOnce("bad-json", "MOOSEDev returned a non-JSON line; skipping this call.")
    }
  })

  let nextID = 1
  const cleanup = () => {
    lines.close()
    child.kill()
  }

  try {
    const initialized = await request(
      child,
      pending,
      nextID++,
      "initialize",
      {
        protocolVersion: "2025-06-18",
        capabilities: {},
        clientInfo: { name: "opencode-moosedev", version: "0.2.0" },
      },
      REQUEST_TIMEOUT_MS,
    )
    if (initialized.error) {
      warnOnce("initialize-error", `MOOSEDev initialize failed; skipping. ${shorten(initialized.error.message || stderr)}`)
      return undefined
    }

    notify(child, "notifications/initialized", {})

    const result = await request(
      child,
      pending,
      nextID++,
      "tools/call",
      { name, arguments: args },
      REQUEST_TIMEOUT_MS,
    )

    if (result.error) {
      warnOnce(`tool-error-${name}`, `${name} failed; skipping. ${shorten(stderr || result.error.message || "")}`)
      return undefined
    }

    if (isToolError(result.result)) {
      warnOnce(`tool-result-error-${name}`, `${name} returned an error result; skipping.`)
      return undefined
    }

    return extractTextContent(result.result)
  } catch (error) {
    warnOnce("mcp-error", `MOOSEDev call failed; skipping. ${shorten(error instanceof Error ? error.message : String(error))}`)
    return undefined
  } finally {
    cleanup()
  }
}

type JsonRpcResponse = {
  id?: number
  result?: unknown
  error?: { message?: string }
}

function isToolError(result: unknown): boolean {
  return Boolean(
    result && typeof result === "object" && (result as Record<string, unknown>).isError === true,
  )
}

function request(
  child: ReturnType<typeof spawn>,
  pending: Map<number, (value: JsonRpcResponse) => void>,
  id: number,
  method: string,
  params: unknown,
  timeoutMs: number,
): Promise<JsonRpcResponse> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      pending.delete(id)
      reject(new Error(`${method} timed out after ${timeoutMs}ms`))
    }, timeoutMs)

    pending.set(id, (response) => {
      clearTimeout(timer)
      pending.delete(id)
      resolve(response)
    })

    child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`, (error) => {
      if (error) {
        clearTimeout(timer)
        pending.delete(id)
        reject(error)
      }
    })
  })
}

function notify(child: ReturnType<typeof spawn>, method: string, params: unknown) {
  child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method, params })}\n`)
}

function extractTextContent(result: unknown): string | undefined {
  if (!result || typeof result !== "object") return undefined
  const content = (result as { content?: unknown }).content
  if (!Array.isArray(content)) return undefined
  const text = content
    .map((item) => {
      if (!item || typeof item !== "object") return ""
      const part = item as { type?: unknown; text?: unknown }
      return part.type === "text" && typeof part.text === "string" ? part.text : ""
    })
    .filter(Boolean)
    .join("\n")
  return text || undefined
}

function oneLine(text: string): string {
  return text.replace(/\s+/g, " ").trim()
}

function shorten(text: string, max = 240): string {
  const clean = oneLine(text)
  if (clean.length <= max) return clean
  return `${clean.slice(0, Math.max(0, max - 3)).trimEnd()}...`
}

/// Cap a multi-line markdown block without collapsing its newlines.
function capBlock(text: string, max: number): string {
  if (text.length <= max) return text
  return `${text.slice(0, Math.max(0, max - 3)).trimEnd()}...`
}
