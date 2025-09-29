return {
  {
    "nvim-tree/nvim-tree.lua",
    enabled = false,
  },
  {
    "stevearc/conform.nvim",
    event = "BufWritePre", -- uncomment for format on save
    opts = require "configs.conform",
  },

  {
    "ellisonleao/glow.nvim",
    config = true,
    cmd = "Glow",
  },

  {
    "williamboman/mason.nvim",
    opts = {
      -- you can put formatters or tools here later if you want
      ensure_installed = { "stylua", "prettier", "shfmt" },
    },
  },

  {
    "ThePrimeagen/harpoon",
    lazy = false,
    dependencies = { "nvim-lua/plenary.nvim" },
    config = true,
    keys = {
      { "<leader>m", "<cmd>lua require('harpoon.mark').add_file()<cr>", desc = "Mark file with harpoon" },
      { "<leader>n", "<cmd>lua require('harpoon.ui').nav_next()<cr>", desc = "Go to next harpoon mark" },
      { "<leader>p", "<cmd>lua require('harpoon.ui').nav_prev()<cr>", desc = "Go to previous harpoon mark" },
      { "<leader>a", "<cmd>lua require('harpoon.ui').toggle_quick_menu()<cr>", desc = "Show harpoon marks" },
    },
  },

  -- Make sure these LSP servers are installed
  {
    "williamboman/mason-lspconfig.nvim",
    opts = {
      ensure_installed = {
        "clangd",
        "lua_ls",
        "qmlls",
        "rust_analyzer",
        "bashls",
        "jsonls",
        "yamlls",
        "html",
        "cssls",
        -- nvim-lspconfig renamed tsserver -> ts_ls recently; we’ll handle both in config
        "tsserver",
        "pyright",
        "dockerls",
        "cmake",
        "codebook",
      },
    },
  },

  -- These are some examples, uncomment them if you want to see them work!
  {
    "neovim/nvim-lspconfig",
    config = function()
      require "configs.lspconfig"
    end,
  },

  {
    "nvim-treesitter/nvim-treesitter",
    opts = {
      ensure_installed = {
        "vim",
        "lua",
        "vimdoc",
        "html",
        "css",
      },
    },
  },

  {
    "CRAG666/code_runner.nvim",
    cmd = { "RunCode", "RunFile", "RunProject", "RunClose" },
    keys = {
      { "<leader>rr", "<cmd>RunCode<CR>", desc = "Run (smart: project/filetype)" },
      { "<leader>rf", "<cmd>RunFile<CR>", desc = "Run current file" },
      { "<leader>rt", "<cmd>RunFile tab<CR>", desc = "Run file in new tab" },
      { "<leader>rp", "<cmd>RunProject<CR>", desc = "Run project command" },
      { "<leader>rc", "<cmd>RunClose<CR>", desc = "Close runner" },
    },
    opts = {
      -- pick where output appears: "float", "tab", "toggleterm", "better_term", "vimux"
      mode = "float",
      focus = true,
      startinsert = true,
      float = { border = "rounded", width = 0.9, x = 0.5, y = 0.5 },

      -- filetype-based commands (extend as you like)
      filetype = {
        python = "python3 -u",
        r = "Rscript",
        sh = "bash",
        lua = "lua",
        javascript = "node",
        typescript = "deno run",
        go = { "cd $dir &&", "go run $file" },
        rust = { "cd $dir &&", "rustc $fileName &&", "$dir/$fileNameWithoutExt" },
        c = function()
          local base = { "cd $dir &&", "gcc $fileName -o", "/tmp/$fileNameWithoutExt" }
          local run = { "&& /tmp/$fileNameWithoutExt &&", "rm /tmp/$fileNameWithoutExt" }
          require("code_runner.commands").run_from_fn(vim.list_extend(base, run))
        end,
        cpp = {
          "cd $dir &&",
          "g++ $fileName -O2 -o /tmp/$fileNameWithoutExt &&",
          "/tmp/$fileNameWithoutExt &&",
          "rm /tmp/$fileNameWithoutExt",
        },
      },
    },
  },

  {
    "michaelb/sniprun",
    build = "sh ./install.sh", -- builds or downloads the binary
    cmd = { "SnipRun", "SnipReset", "SnipClose", "SnipInfo", "SnipReplMemoryClean", "SnipLive" },
    keys = {
      { "<leader>rs", ":'<,'>SnipRun<CR>", mode = "v", desc = "Run selection" },
      { "<leader>rl", ":SnipRun<CR>", mode = "n", desc = "Run current line" },
      { "<leader>rx", ":SnipClose<CR>", mode = "n", desc = "Close Sniprun UI" },
    },
    opts = {
      display = {
        "Terminal",
      },
      show_no_output = {
        "Classic",
        "TempFloatingWindow",
      },
      display_options = {
        terminal_position = "horizontal",
        terminal_height = 15,
      },
    },
  },
}
