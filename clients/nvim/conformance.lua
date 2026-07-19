-- MOOSEDev Knowledge-LSP conformance suite (spec §5.7/§7): drives a REAL
-- Neovim LSP client headlessly against the daemon and asserts the full v2
-- protocol surface — capabilities, hover, code lens, pull diagnostics, code
-- actions, and an executeCommand write-path round-trip (idempotency
-- included). Exits non-zero on any failure.
--
--   nvim --headless -l conformance.lua <repo_root> <rel_file> <line> <col>
--
-- <line>/<col> are 1-based and must sit on a substrate-resolved public
-- definition with linked knowledge, because a non-null dossier hover is part
-- of this fixture's contract. Run via conformance.sh, which prepares a SCRATCH
-- copy of the repo so the filed proposal never lands in a real project graph.
--
-- API notes: `vim.lsp.buf_request_sync` is the stable sync-request surface
-- across 0.9–0.12; params are built by hand (make_position_params grew a
-- position_encoding argument mid-series).

local repo = arg[1]
local relfile = arg[2]
local line = tonumber(arg[3])
local col = tonumber(arg[4])
if not (repo and relfile and line and col) then
  io.stderr:write("usage: nvim --headless -l conformance.lua <repo_root> <rel_file> <line> <col>\n")
  os.exit(2)
end

local failures = 0
local function check(cond, msg)
  if cond then
    io.write("ok: " .. msg .. "\n")
  else
    failures = failures + 1
    io.stderr:write("FAIL: " .. msg .. "\n")
  end
  return cond
end

-- 1. Start the real client (the stdio shim autospawns the daemon; a fresh
--    scratch store hydrates from kg.nq, so allow a generous first-init wait).
--    The showDocument handler answers the server's openEntity round-trip
--    without actually opening a browser; the publishDiagnostics counter
--    proves push suppression for this pull-capable client.
local show_document_requests = {}
local publish_count = 0
local client_id = vim.lsp.start({
  name = "moosedev-conformance",
  cmd = { "moosedev", "lsp" },
  root_dir = repo,
  handlers = {
    ["window/showDocument"] = function(_, result)
      table.insert(show_document_requests, result)
      return { success = true }
    end,
    ["textDocument/publishDiagnostics"] = function()
      publish_count = publish_count + 1
    end,
  },
})
if not check(client_id ~= nil, "client started") then
  os.exit(1)
end
vim.wait(180000, function()
  local c = vim.lsp.get_clients({ id = client_id })[1]
  return c ~= nil and c.initialized
end, 200)
local client = vim.lsp.get_clients({ id = client_id })[1]
if not check(client ~= nil and client.initialized, "client initialized") then
  os.exit(1)
end

-- 2. Capability conformance (v2.0 read surface + v2.3 write path).
local caps = client.server_capabilities
check(caps.hoverProvider == true, "capability: hoverProvider")
check(caps.codeLensProvider ~= nil, "capability: codeLensProvider")
check(caps.codeActionProvider ~= nil, "capability: codeActionProvider")
check(
  caps.executeCommandProvider ~= nil
    and vim.tbl_contains(caps.executeCommandProvider.commands, "moosedev.proposeLink")
    and vim.tbl_contains(caps.executeCommandProvider.commands, "moosedev.proposeJudgment")
    and vim.tbl_contains(caps.executeCommandProvider.commands, "moosedev.openEntity"),
  "capability: executeCommandProvider commands"
)
check(caps.diagnosticProvider ~= nil, "capability: diagnosticProvider (pull)")

-- 3. Attach a real buffer at the target position.
vim.cmd.edit(repo .. "/" .. relfile)
local bufnr = vim.api.nvim_get_current_buf()
vim.lsp.buf_attach_client(bufnr, client_id)
vim.wait(500)
local uri = vim.uri_from_bufnr(bufnr)
local pos = { line = line - 1, character = col - 1 }

local function req(method, params, label)
  local responses = vim.lsp.buf_request_sync(bufnr, method, params, 15000)
  local entry = responses and responses[client_id]
  if entry and entry.error then
    io.stderr:write(label .. " error: " .. vim.inspect(entry.error) .. "\n")
    return nil, entry.error
  end
  return entry and entry.result, nil
end

-- 4. Hover: this knowledge-bearing conformance target must return a dossier.
local hover, hover_err = req("textDocument/hover", {
  textDocument = { uri = uri },
  position = pos,
}, "hover")
check(hover_err == nil, "hover answers without error")
check(hover ~= nil and hover.contents ~= nil, "hover returns a non-null dossier")
check(
  hover ~= nil
    and hover.contents ~= nil
    and type(hover.contents.value) == "string"
    and hover.contents.value:find("**", 1, true) ~= nil,
  "hover renders dossier markdown"
)

-- 5. Code lens.
local lenses, lens_err = req("textDocument/codeLens", { textDocument = { uri = uri } }, "codeLens")
check(lens_err == nil and type(lenses) == "table", "codeLens replies with a list")

-- 6. Pull diagnostics (LSP 3.17): a full report.
local diag, diag_err = req("textDocument/diagnostic", { textDocument = { uri = uri } }, "diagnostic")
check(
  diag_err == nil and diag ~= nil and diag.kind == "full" and type(diag.items) == "table",
  "pull diagnostics returns a full report"
)

-- 6b. Lens command round-trip: `moosedev.openEntity` is server-handled — the
--     daemon resolves the workbench URL and asks this client to open it via
--     window/showDocument (answered by the handler above, browserless).
--     Neovim advertises window.showDocument from 0.10.
local open_lens
for _, lens in ipairs(lenses or {}) do
  if lens.command and lens.command.command == "moosedev.openEntity" and lens.command.arguments ~= nil then
    open_lens = lens.command
    break
  end
end
if check(open_lens ~= nil, "a lens carries moosedev.openEntity with an entity argument")
  and vim.fn.has("nvim-0.10") == 1
then
  local _, open_err = req("workspace/executeCommand", {
    command = open_lens.command,
    arguments = open_lens.arguments,
  }, "executeCommand openEntity")
  check(open_err == nil, "openEntity answers without error")
  vim.wait(2000, function()
    return #show_document_requests > 0
  end, 50)
  local shown = show_document_requests[1]
  check(shown ~= nil and shown.external == true, "server sent window/showDocument (external)")
  check(
    shown ~= nil and type(shown.uri) == "string" and shown.uri:find("/#/record/", 1, true) ~= nil,
    "showDocument opens a workbench record URL"
  )
end

-- 7. Code action: the lightbulb menu on the target entity.
local actions, action_err = req("textDocument/codeAction", {
  textDocument = { uri = uri },
  range = { start = pos, ["end"] = pos },
  context = { diagnostics = {} },
}, "codeAction")
check(action_err == nil and type(actions) == "table", "codeAction replies with a list")

local propose
for _, action in ipairs(actions or {}) do
  if action.command and action.command.command:find("^moosedev%.propose") then
    propose = action.command
    if action.command.command == "moosedev.proposeLink" then
      break -- prefer the link path: it exercises candidate search + re-resolution
    end
  end
end
if check(propose ~= nil, "menu offers a moosedev.propose* action at the target position") then
  -- 8. The write path: executeCommand files a proposal into the ratification
  --    queue and repeating it is idempotent (the pending twin returns).
  local first, exec_err = req("workspace/executeCommand", {
    command = propose.command,
    arguments = propose.arguments,
  }, "executeCommand")
  check(
    exec_err == nil and first ~= nil and type(first.proposalIri) == "string",
    "executeCommand files a proposal (" .. propose.command .. ")"
  )
  local second = req("workspace/executeCommand", {
    command = propose.command,
    arguments = propose.arguments,
  }, "executeCommand repeat")
  check(
    first ~= nil and second ~= nil and second.proposalIri == first.proposalIri,
    "repeated executeCommand is idempotent"
  )
  -- 9. Invalid arguments are refused, never silently filed.
  local _, invalid = req("workspace/executeCommand", {
    command = "moosedev.proposeJudgment",
    arguments = { { entityIri = "not an iri", predicate = "playsRole", targetLocal = "boundary" } },
  }, "executeCommand invalid")
  check(invalid ~= nil and invalid.code == -32602, "malformed arguments get InvalidParams")
end

-- 10. Push suppression: this client advertises textDocument.diagnostic
--     (0.10+), so the daemon must serve pull ONLY — a push would land in a
--     second namespace and double-report every squiggle.
if vim.fn.has("nvim-0.10") == 1 then
  check(
    publish_count == 0,
    "no publishDiagnostics pushed to a pull-capable client (saw " .. publish_count .. ")"
  )
end

-- client:stop() on 0.11+; the module-level form for older Neovim.
if type(client.stop) == "function" then
  client:stop()
else
  vim.lsp.stop_client(client_id)
end
vim.wait(1000)
io.write(failures == 0 and "conformance: PASS\n" or ("conformance: " .. failures .. " failure(s)\n"))
os.exit(failures == 0 and 0 or 1)
