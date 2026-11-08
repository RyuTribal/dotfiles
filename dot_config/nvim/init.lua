vim.g.base46_cache = vim.fn.stdpath "data" .. "/base46/"
vim.g.mapleader = " "
-- hey
-- bootstrap lazy and all plugins
local lazypath = vim.fn.stdpath "data" .. "/lazy/lazy.nvim"

if not vim.uv.fs_stat(lazypath) then
  local repo = "https://github.com/folke/lazy.nvim.git"
  vim.fn.system { "git", "clone", "--filter=blob:none", repo, "--branch=stable", lazypath }
end

vim.opt.rtp:prepend(lazypath)

local lazy_config = require "configs.lazy"

-- load plugins
require("lazy").setup({
  {
    "NvChad/NvChad",
    lazy = false,
    branch = "v2.5",
    import = "nvchad.plugins",
  },

  { import = "plugins" },
}, lazy_config)

_G.AxonThemeReload = function()
  require("nvchad.utils").reload("themes.axon")   -- module arg is required, VERIFIED utils.lua:53-65
  return "ok"
end
-- startup: recompile the base46 cache if the theme changed while no nvim ran; without a generation,
-- warn once and skip the check
local current = (os.getenv("XDG_STATE_HOME") or (os.getenv("HOME") .. "/.local/state")) .. "/axon/theme/current/"
local ok, m = pcall(function()
  return vim.json.decode(table.concat(vim.fn.readfile(current .. "manifest.json"), ""))
end)
if ok then
  local stamp_file = vim.g.base46_cache .. "axon.stamp"
  local want = m.outputs["nvim-base46.lua"].since
  if (vim.fn.filereadable(stamp_file) == 1 and vim.fn.readfile(stamp_file)[1] or "") ~= want then
    require("base46").load_all_highlights()
    vim.fn.writefile({ want }, stamp_file)
  end
else
  vim.notify("axon: no theme generation at " .. current .. "; keeping the compiled colours (onedark if none)",
    vim.log.levels.WARN)
end

-- load theme
dofile(vim.g.base46_cache .. "defaults")
dofile(vim.g.base46_cache .. "statusline")

require "options"
require "autocmds"

vim.schedule(function()
  require "mappings"
end)
