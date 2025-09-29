local options = {
  formatters_by_ft = {
    lua = { "stylua" },
    javascript = { "prettier" },
    javascriptreact = { "prettier" },
    typescript = { "prettier" },
    typescriptreact = { "prettier" },
    json = { "prettier" },
    yaml = { "prettier" },
    html = { "prettier" },
    css = { "prettier" },
    sh = { "shfmt" },
    rust = { "rustfmt" },
    c = { "clang-format" },
    cpp = { "clang-format" },
    go = { "goimports" },
    markdown = { "prettier" },
  },

  -- Turn on format-on-save
  format_on_save = function(bufnr)
    -- Let you disable globally or per-buffer if needed
    if vim.g.disable_autoformat or vim.b[bufnr].disable_autoformat then
      return
    end
    return { lsp_fallback = true, timeout_ms = 500 }
  end,

  notify_on_error = true,
}

return options
