require "nvchad.mappings"

-- add yours here
--
local map = vim.keymap.set
map("n", ";", ":", { desc = "CMD enter command mode" })
map("i", "jk", "<ESC>")
map("n", "<leader>mp", ":Glow<CR>", { silent = true, desc = "Markdown Preview" })

vim.keymap.set("n", "<leader>e", function()
  vim.diagnostic.open_float(nil, { focus = false, scope = "cursor" })
end, { desc = "Show diagnostic at cursor" })

vim.keymap.set("n", "<leader>fc", function()
  vim.lsp.buf.code_action {
    apply = true,
  }
end, { desc = "LSP: Fix all (if available)" })

vim.keymap.set("n", "<leader>s", ":nohlsearch<CR>", { desc = "Clear search highlight" })

local nomap = vim.keymap.del
nomap("n", "<Tab>")
nomap("n", "<S-Tab>")
nomap("n", "<leader>h")
nomap("n", "<leader>n")

-- map("n", "<Tab>", "<Nop>", { desc = "disable tab buffer-next" })
-- map("n", "<S-Tab>", "<Nop>", { desc = "disable shift-tab buffer-prev" })
-- map({ "n", "i", "v" }, "<C-s>", "<cmd> w <cr>")
