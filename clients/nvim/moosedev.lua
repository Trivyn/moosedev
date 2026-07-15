-- MOOSEDev Knowledge-LSP registration for Neovim (spec §5.7: a plain
-- lspconfig entry — no plugin required).
--
-- The server is the `moosedev` binary's stdio shim; it relays to the shared
-- daemon and autospawns it on first use. Language intelligence stays with
-- rust-analyzer / typescript-language-server — Neovim attaches multiple
-- servers per buffer, and MOOSEDev only adds hover dossiers, code lenses,
-- knowledge diagnostics, and the proposal code actions.
--
-- Neovim 0.11+ (vim.lsp.config): drop this file into your config (e.g.
-- require it from init.lua, or copy the body).

vim.lsp.config("moosedev", {
  cmd = { "moosedev", "lsp" },
  filetypes = { "rust", "typescript", "typescriptreact" },
  root_markers = { ".moosedev" },
})
vim.lsp.enable("moosedev")

-- Neovim 0.9/0.10 (classic nvim-lspconfig) equivalent:
--
--   local configs = require("lspconfig.configs")
--   if not configs.moosedev then
--     configs.moosedev = {
--       default_config = {
--         cmd = { "moosedev", "lsp" },
--         filetypes = { "rust", "typescript", "typescriptreact" },
--         root_dir = require("lspconfig.util").root_pattern(".moosedev"),
--       },
--     }
--   end
--   require("lspconfig").moosedev.setup({})
