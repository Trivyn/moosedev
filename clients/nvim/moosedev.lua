-- MOOSEDev Knowledge-LSP registration for Neovim (spec §5.7: a plain
-- lspconfig entry — no plugin required).
--
-- The server is the `moosedev` binary's stdio shim; it relays to the shared
-- daemon and autospawns it on first use. Language intelligence stays with
-- rust-analyzer / typescript-language-server — Neovim attaches multiple
-- servers per buffer, and MOOSEDev only adds hover dossiers, code lenses,
-- knowledge diagnostics, and the proposal code actions.
--
-- No `filetypes` list: attach is gated by the `.moosedev` root marker, and
-- the server is silent for files outside its indexing substrate — so new
-- substrate languages need no client edit (the supported set lives in
-- src/code/substrate/lang/).
--
-- Neovim 0.11+ (vim.lsp.config): drop this file into your config (e.g.
-- require it from init.lua, or copy the body).

vim.lsp.config("moosedev", {
  cmd = { "moosedev", "lsp" },
  root_markers = { ".moosedev" },
  -- Load-bearing with no `filetypes` filter: without it, buffers outside any
  -- MOOSEDev project would start the server with root_dir=nil and the shim
  -- would autospawn a daemon (creating .moosedev state) in Neovim's cwd.
  workspace_required = true,
})
vim.lsp.enable("moosedev")

-- Code lenses (knowledge badges) render on demand; Neovim does not refresh
-- them automatically. Optional autocmd:
--
--   vim.api.nvim_create_autocmd({ "BufEnter", "CursorHold", "InsertLeave" }, {
--     callback = function(args)
--       for _ in pairs(vim.lsp.get_clients({ bufnr = args.buf, name = "moosedev" })) do
--         vim.lsp.codelens.refresh({ bufnr = args.buf })
--       end
--     end,
--   })
--
-- Run a lens with `vim.lsp.codelens.run()`; MOOSEDev handles the command
-- server-side and opens the workbench via window/showDocument.

-- Neovim 0.9/0.10 (classic nvim-lspconfig) equivalent: autostart needs an
-- explicit filetypes list there — keep it matching src/code/substrate/lang/.
--
--   local configs = require("lspconfig.configs")
--   if not configs.moosedev then
--     configs.moosedev = {
--       default_config = {
--         cmd = { "moosedev", "lsp" },
--         filetypes = { "rust", "typescript", "typescriptreact", "javascript",
--                       "javascriptreact", "python" },
--         root_dir = require("lspconfig.util").root_pattern(".moosedev"),
--         -- Never start without a .moosedev root (same guard as
--         -- workspace_required above).
--         single_file_support = false,
--       },
--     }
--   end
--   require("lspconfig").moosedev.setup({})
