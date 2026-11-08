-- ~/.config/nvim/lua/themes/axon.lua (static, chezmoi-managed)
local path = (os.getenv("XDG_STATE_HOME") or (os.getenv("HOME") .. "/.local/state"))
  .. "/axon/theme/current/nvim-base46.lua"
if vim.uv.fs_stat(path) then return dofile(path) end
return require("base46.themes.onedark")   -- base46's default theme; init.lua has already warned
