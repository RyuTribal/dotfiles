local nvlsp = require "nvchad.configs.lspconfig"
nvlsp.defaults()

-- 2) Your server list (rename tsserver -> ts_ls if you’re on newer lspconfig)
local servers = {
  "clangd", -- C/C++
  "lua_ls", -- Lua
  "qmlls", -- QML
  "rust_analyzer",
  "bashls",
  "jsonls",
  "yamlls",
  "html",
  "cssls",
  "ts_ls", -- TypeScript / JavaScript (was "tsserver")
  "pyright",
  "dockerls",
  "cmake",
  "gopls",
  "glsl_analyzer",
  -- "codebook",   -- <-- remove or replace; not an lspconfig server
}

local lsp = vim.lsp
if not (lsp and lsp.config and lsp.enable) then
  vim.notify("vim.lsp.config API unavailable (need Neovim 0.11+)", vim.log.levels.ERROR)
  return
end

-- apply NvChad defaults to every LSP
lsp.config("*", {
  on_attach = nvlsp.on_attach,
  capabilities = nvlsp.capabilities,
})

local per_server = {
  clangd = {
    cmd = {
      "clangd",
      "--pch-storage=disk",
      "--background-index",
    },
  },
}

local function has_server(name)
  local ok, cfg = pcall(function()
    return lsp.config[name]
  end)
  return ok and type(cfg) == "table"
end

for _, srv in ipairs(servers) do
  if not has_server(srv) then
    vim.notify(("vim.lsp.config: unknown server '%s' (skipping)"):format(srv), vim.log.levels.WARN)
  else
    local override = per_server[srv]
    if override then
      lsp.config(srv, override)
    end
    lsp.enable(srv)
  end
end
