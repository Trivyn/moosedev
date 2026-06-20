import { spawn } from "node:child_process"
import { createInterface } from "node:readline"

type PluginInput = {
  directory: string
  worktree?: string
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

type SystemTransformOutput = {
  system: string[]
}

type MessagesTransformOutput = {
  messages: Array<{
    info: unknown
    parts: Array<Record<string, unknown>>
  }>
}

type KnowledgeRecord = {
  title: string
  description: string
  iri: string
}

const MAX_WORKING_SET = 8
const MAX_SYMBOLS = 6
const RETRIEVAL_LIMIT = 5
const BLOCK_LIMIT = 8
const REQUEST_TIMEOUT_MS = 5_000
const BLOCK_HEADER = "## Relevant recorded project knowledge (MOOSEDev)"
const IRI_PATTERN = /https:\/\/moosedev\.dev\/kg\/\S+/g
// Definition names (struct/enum/trait/type/class) are the strongest retrieval keys — records are
// indexed by SYMBOL (e.g. "SchemaTriple"), NOT by file path. CamelCase-initial only, and NO `impl`
// (which captures the trait — "impl Default for X" → "Default" — i.e. noise).
const TYPE_DEF_RE = /\b(?:struct|enum|trait|type|class|interface)\s+([A-Z][A-Za-z0-9_]*)/g
// Common std/derive names that are noise as retrieval keys.
const SYMBOL_DENYLIST = new Set([
  "Default", "Clone", "Debug", "Serialize", "Deserialize", "Copy", "Eq", "PartialEq", "Ord",
  "PartialOrd", "Hash", "From", "Into", "TryFrom", "TryInto", "Display", "Error", "Iterator",
  "Send", "Sync", "Sized", "Drop", "Self", "Option", "Result", "Vec", "String", "Box", "Arc",
])
const PATH_ARG_KEYS = [
  "filePath",
  "filepath",
  "path",
  "absolutePath",
  "relativePath",
  "files",
  "paths",
]

export async function MooseDevPush(input: PluginInput) {
  const root = input.worktree || input.directory || process.cwd()
  const workingSetBySession = new Map<string, string[]>()
  const symbolsBySession = new Map<string, string[]>()
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

  function symbolSet(sessionID: string) {
    let set = symbolsBySession.get(sessionID)
    if (!set) {
      set = []
      symbolsBySession.set(sessionID, set)
    }
    return set
  }

  function addSymbols(sessionID: string, symbols: string[]) {
    const set = symbolSet(sessionID)
    const defaultSet = sessionID === "default" ? set : symbolSet("default")
    for (const sym of symbols) {
      pushRecent(set, sym)
      if (defaultSet !== set) pushRecent(defaultSet, sym)
    }
  }

  async function knowledgeBlock(sessionID: string): Promise<string | undefined> {
    // Aggregate the session's set with the "default" accumulator: opencode may invoke this hook with a
    // sessionID different from the one tool.execute.after observed, so rely on the union, not a match.
    const symbols = [...new Set([...symbolSet(sessionID), ...symbolSet("default")])].slice(0, MAX_SYMBOLS)
    const paths = [...new Set([...workingSet(sessionID), ...workingSet("default")])]

    // Retrieve PER-SYMBOL and merge. A combined multi-symbol topic dilutes the specific record out of
    // the top-K (e.g. SchemaTriple's cache-gap lesson gets buried by ingest symbols), so query each
    // focused symbol separately and union the results; fall back to a path-based topic if no symbols.
    const pathTopic = buildTopic([], paths)
    const queries = symbols.length > 0 ? symbols : pathTopic ? [pathTopic] : []
    if (queries.length === 0) return undefined

    const perSymbol: KnowledgeRecord[][] = []
    for (const q of queries) {
      const text = await getRelevantContext(q, 2, warnOnce)
      perSymbol.push(text ? parseRecords(text) : [])
    }
    // Round-robin merge: take each focused symbol's rank-1 record FIRST (so every symbol contributes
    // its best hit before any rank-2 fills the block), then rank-2. Guarantees the edit-target
    // symbol's top record (e.g. SchemaTriple's cache-gap lesson) survives even amid noisier symbols.
    const seen = new Set<string>()
    const records: KnowledgeRecord[] = []
    for (let rank = 0; rank < 2 && records.length < BLOCK_LIMIT; rank++) {
      for (const recs of perSymbol) {
        const rec = recs[rank]
        if (rec && !seen.has(rec.iri)) {
          seen.add(rec.iri)
          records.push(rec)
        }
        if (records.length >= BLOCK_LIMIT) break
      }
    }

    // Inject the CURRENT records every turn. The block is re-rendered per turn (prepended to a fresh
    // system prompt), so it does not grow across turns.
    if (records.length === 0) return undefined
    return renderBlock(records)
  }

  return {
    "tool.execute.after": async (toolInput: ToolAfterInput, output: ToolAfterOutput) => {
      const paths = extractToolPaths(toolInput.tool, toolInput.args, output.output)
      if (paths.length > 0) addWorkingPaths(toolInput.sessionID, paths)
      const symbols = extractSymbols(`${safeStringify(toolInput.args)}\n${output.output || ""}`)
      if (symbols.length > 0) addSymbols(toolInput.sessionID, symbols)
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

function pushRecent(set: string[], value: string) {
  const existing = set.indexOf(value)
  if (existing >= 0) set.splice(existing, 1)
  set.unshift(value)
  if (set.length > MAX_WORKING_SET) set.length = MAX_WORKING_SET
}

function buildTopic(symbols: string[], paths: string[]): string | undefined {
  // Lead with SYMBOLS (records are symbol-indexed); add a few recent file basenames as backup.
  const symbolBits = symbols.slice(0, MAX_SYMBOLS)
  const fileBits = paths.slice(0, 3).flatMap((path) => {
    const basename = path.split("/").at(-1)
    return basename ? [basename] : []
  })
  const parts = [...symbolBits, ...fileBits]
  if (parts.length === 0) return undefined
  return parts.join(" ").slice(0, 240)
}

function extractSymbols(text: string): string[] {
  const out: string[] = []
  TYPE_DEF_RE.lastIndex = 0
  let match: RegExpExecArray | null
  while ((match = TYPE_DEF_RE.exec(text))) {
    if (!SYMBOL_DENYLIST.has(match[1])) out.push(match[1])
  }
  return out
}

function safeStringify(value: unknown): string {
  try {
    return typeof value === "string" ? value : JSON.stringify(value) ?? ""
  } catch {
    return ""
  }
}

async function getRelevantContext(
  topic: string,
  limit: number,
  warnOnce: (key: string, message: string) => void,
): Promise<string | undefined> {
  const dataDir = process.env.MOOSEDEV_DATA_DIR
  if (!dataDir) {
    warnOnce("missing-data-dir", "MOOSEDEV_DATA_DIR is unset; skipping project-memory injection.")
    return undefined
  }

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
      warnOnce("bad-json", "MOOSEDev returned a non-JSON line; skipping injection for this turn.")
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
        clientInfo: { name: "opencode-moosedev-push", version: "0.1.0" },
      },
      REQUEST_TIMEOUT_MS,
    )
    if (initialized.error) {
      warnOnce("initialize-error", `MOOSEDev initialize failed; skipping injection. ${shorten(initialized.error.message || stderr)}`)
      return undefined
    }

    notify(child, "notifications/initialized", {})

    const result = await request(
      child,
      pending,
      nextID++,
      "tools/call",
      { name: "get_relevant_context", arguments: { topic, limit } },
      REQUEST_TIMEOUT_MS,
    )

    if (result.error) {
      warnOnce("tool-error", `get_relevant_context failed; skipping injection. ${shorten(stderr || result.error.message || "")}`)
      return undefined
    }

    return extractTextContent(result.result)
  } catch (error) {
    warnOnce("mcp-error", `MOOSEDev retrieval failed; skipping injection. ${shorten(error instanceof Error ? error.message : String(error))}`)
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

function parseRecords(text: string): KnowledgeRecord[] {
  const records: KnowledgeRecord[] = []
  let current: Partial<KnowledgeRecord> | undefined

  for (const line of text.split(/\r?\n/)) {
    const title = line.match(/^•\s+.+?\s+—\s+"(.+)"\s*$/)
    if (title) {
      if (isCompleteRecord(current)) records.push(current)
      current = { title: title[1], description: "" }
      continue
    }

    if (!current) continue

    const description = line.match(/^\s+hasDescription:\s*(.+)$/)
    if (description) {
      current.description = description[1]
      continue
    }

    const iri = line.match(IRI_PATTERN)
    if (iri) {
      current.iri = iri[0]
    }
  }

  if (isCompleteRecord(current)) records.push(current)
  return records
}

function isCompleteRecord(record: Partial<KnowledgeRecord> | undefined): record is KnowledgeRecord {
  return Boolean(record?.title && record.iri)
}

function renderBlock(records: KnowledgeRecord[]): string {
  const lines = records.map((record) => {
    const title = shorten(oneLine(record.title), 160)
    const description = shorten(oneLine(record.description), 420)
    return description ? `- ${title}: ${description}` : `- ${title}`
  })
  return `${BLOCK_HEADER}\n${lines.join("\n")}`
}

function oneLine(text: string): string {
  return text.replace(/\s+/g, " ").trim()
}

function shorten(text: string, max = 240): string {
  const clean = oneLine(text)
  if (clean.length <= max) return clean
  return `${clean.slice(0, Math.max(0, max - 3)).trimEnd()}...`
}
