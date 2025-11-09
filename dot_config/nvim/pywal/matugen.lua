local M = {}

local lighten = require("base46.colors").change_hex_lightness

local seq_path = (os.getenv("XDG_STATE_HOME") or (os.getenv("HOME") .. "/.local/state"))
  .. "/quickshell/user/generated/terminal/sequences.txt"

local function read_osc_palette(path)
  local f = io.open(path, "rb")
  if not f then
    return nil
  end

  local raw = f:read("*a")
  f:close()

  if not raw then
    return nil
  end

  local palette = {}
  raw = raw:gsub("\027", "\n")
  for token in raw:gmatch("[^\n]+") do
    local idx, hex = token:match("%]4;(%d+);#(%x%x%x%x%x%x)")
    if idx and hex then
      idx = tonumber(idx)
      if idx and idx >= 0 and idx <= 15 then
        palette["term" .. idx] = "#" .. hex:lower()
      end
    end
  end

  if next(palette) == nil then
    return nil
  end

  return palette
end

local term_palette = read_osc_palette(seq_path)
local term_bg = term_palette and term_palette.term0
local term_fg = term_palette and term_palette.term7

term_bg = term_bg or "{{colors.background.default.hex}}"
term_fg = term_fg or "{{colors.on_background.default.hex}}"

M.base_30 = {
  white = term_fg,
  black = term_bg,
  darker_black = lighten(term_bg, -3),
  black2 = lighten(term_bg, 6),
  one_bg = lighten(term_bg, 10),
  one_bg2 = lighten(term_bg, 16),
  one_bg3 = lighten(term_bg, 22),
  grey = "{{colors.surface_variant.default.hex}}",
  grey_fg = lighten("{{colors.surface_variant.default.hex}}", -10),
  grey_fg2 = lighten("{{colors.surface_variant.default.hex}}", -20),
  light_grey = "{{colors.outline.default.hex}}",
  red = "{{colors.red.default.hex}}",
  baby_pink = lighten("{{colors.red.default.hex}}", 10),
  pink = "{{colors.tertiary.default.hex}}",
  line = "{{colors.outline.default.hex}}",
  green = "{{colors.green.default.hex}}",
  vibrant_green = lighten("{{colors.green.default.hex}}", 10),
  blue = "{{colors.blue.default.hex}}",
  nord_blue = lighten("{{colors.blue.default.hex}}", 10),
  yellow = "{{colors.yellow.default.hex}}",
  sun = lighten("{{colors.yellow.default.hex}}", 10),
  purple = "{{colors.tertiary.default.hex}}",
  dark_purple = lighten("{{colors.tertiary.default.hex}}", -10),
  teal = "{{colors.secondary_container.default.hex}}",
  orange = "{{colors.red.default.hex}}",
  cyan = "{{colors.cyan.default.hex}}",
  statusline_bg = lighten(term_bg, 6),
  pmenu_bg = "{{colors.surface_variant.default.hex}}",
  folder_bg = lighten("{{colors.primary_fixed_dim.default.hex}}", 0),
  lightbg = lighten(term_bg, 10),
}

M.base_16 = {
  base00 = term_bg,
  base01 = "{{colors.surface_container_low.default.hex}}",
  base02 = lighten("{{colors.surface_variant.default.hex}}", 3),
  base03 = lighten("{{colors.outline.default.hex}}", 0),
  base04 = term_fg,
  base05 = term_fg,
  base06 = "{{colors.surface_bright.default.hex}}",
  base07 = "{{colors.on_surface.default.hex}}",
  base08 = "{{colors.red.default.hex}}",
  base09 = "{{colors.yellow.default.hex}}",
  base0A = "{{colors.blue.default.hex}}",
  base0B = "{{colors.green.default.hex}}",
  base0C = "{{colors.cyan.default.hex}}",
  base0D = lighten("{{colors.blue.default.hex}}", 20),
  base0E = "{{colors.tertiary.default.hex}}",
  base0F = "{{colors.inverse_surface.default.hex}}",
}

M.type = "dark"

M.polish_hl = {
  defaults = {
    Comment = {
      italic = true,
      fg = M.base_16.base03,
    },
  },
  Syntax = {
    String = {
      fg = "{{colors.tertiary.default.hex}}",
    },
  },
  treesitter = {
    ["@comment"] = {
      fg = M.base_16.base03,
    },
    ["@string"] = {
      fg = "{{colors.tertiary.default.hex}}",
    },
  },
}

return M
