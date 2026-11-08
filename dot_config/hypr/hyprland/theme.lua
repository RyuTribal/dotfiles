-- ~/.config/hypr/hyprland/theme.lua (static; axon writes only the two files it loads)
local base = (os.getenv("XDG_STATE_HOME") or (os.getenv("HOME") .. "/.local/state")) .. "/axon/"
local look = { config = hl.config, curve = hl.curve, animation = hl.animation,
               window_rule = hl.window_rule, layer_rule = hl.layer_rule, workspace_rule = hl.workspace_rule }
-- pure helpers only: no io, require, load, os.execute or hl.exec_cmd/bind/env/on.
-- os.getenv is the one os function, read-only, for the instance guard at the top of session.lua (section 30).
local env = { hl = look, os = { getenv = os.getenv }, ipairs = ipairs, pairs = pairs, string = string,
              table = table, math = math, tostring = tostring, tonumber = tonumber }
-- run one file in the sandbox; returns an error string or nil
local function run(path, required)
  local file = io.open(path, "r")           -- the loader itself runs with Hyprland's full Lua, so io is here
  if not file then
    if required then return "no generation at " .. path .. "; Hyprland defaults until axond renders one" end
    return nil                              -- session.lua absent: nothing to restore
  end
  file:close()
  local chunk, err = loadfile(path, "t", env)
  if not chunk then return err end
  local ok, run_error = pcall(chunk)
  if not ok then return path .. ": " .. tostring(run_error) end
  return nil
end
local errors = {}
for _, item in ipairs({ { path = base .. "theme/current/hyprland.lua", required = true },
                        { path = base .. "hypr/session.lua", required = false } }) do
  local item_error = run(item.path, item.required)
  if item_error then errors[#errors + 1] = item_error end
end
if #errors > 0 then error("axon theme loader: " .. table.concat(errors, "; ")) end
