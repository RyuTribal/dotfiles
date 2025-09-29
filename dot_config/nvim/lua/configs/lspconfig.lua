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
  -- "codebook",   -- <-- remove or replace; not an lspconfig server
}

local lspconfig = require "lspconfig"

-- tiny helper to skip unknown servers
local function has_server(name)
  return type(lspconfig[name]) == "table" and type(lspconfig[name].setup) == "function"
end

for _, srv in ipairs(servers) do
  if not has_server(srv) then
    vim.notify(("lspconfig: unknown server '%s' (skipping)"):format(srv), vim.log.levels.WARN)
  elseif srv == "clangd" then
    -- per-server overrides + NvChad defaults
    lspconfig.clangd.setup {
      on_attach = nvlsp.on_attach,
      capabilities = nvlsp.capabilities,
      cmd = {
        "clangd",
        "--pch-storage=disk",
        "--malloc-trim",
        "-j=2",
        "--limit-results=100",
        "--limit-references=500",
        "--background-index",
      },
    }
  else
    -- vanilla setup using NvChad defaults
    lspconfig[srv].setup {
      on_attach = nvlsp.on_attach,
      capabilities = nvlsp.capabilities,
    }
  end
end
