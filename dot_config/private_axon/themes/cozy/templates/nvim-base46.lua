{#- base46's theme table for NvChad: today's pywal/matugen.lua, with the terminal background
    and foreground taken from the palette instead of sequences.txt and the type following the
    mode. lighten() is base46's own, run when nvim compiles the theme. -#}
local M = {}

local lighten = require("base46.colors").change_hex_lightness

local term_bg = "{{ term.bg.hex }}"
local term_fg = "{{ term.fg.hex }}"

M.base_30 = {
  white = term_fg,
  black = term_bg,
  darker_black = lighten(term_bg, -3),
  black2 = lighten(term_bg, 6),
  one_bg = lighten(term_bg, 10),
  one_bg2 = lighten(term_bg, 16),
  one_bg3 = lighten(term_bg, 22),
  grey = "{{ colors.surface_variant.hex }}",
  grey_fg = lighten("{{ colors.surface_variant.hex }}", -10),
  grey_fg2 = lighten("{{ colors.surface_variant.hex }}", -20),
  light_grey = "{{ colors.outline.hex }}",
  red = "{{ colors.red.hex }}",
  baby_pink = lighten("{{ colors.red.hex }}", 10),
  pink = "{{ colors.tertiary.hex }}",
  line = "{{ colors.outline.hex }}",
  green = "{{ colors.green.hex }}",
  vibrant_green = lighten("{{ colors.green.hex }}", 10),
  blue = "{{ colors.blue.hex }}",
  nord_blue = lighten("{{ colors.blue.hex }}", 10),
  yellow = "{{ colors.yellow.hex }}",
  sun = lighten("{{ colors.yellow.hex }}", 10),
  purple = "{{ colors.tertiary.hex }}",
  dark_purple = lighten("{{ colors.tertiary.hex }}", -10),
  teal = "{{ colors.secondary_container.hex }}",
  orange = "{{ colors.red.hex }}",
  cyan = "{{ colors.cyan.hex }}",
  statusline_bg = lighten(term_bg, 6),
  pmenu_bg = "{{ colors.surface_variant.hex }}",
  folder_bg = lighten("{{ colors.primary_fixed_dim.hex }}", 0),
  lightbg = lighten(term_bg, 10),
}

M.base_16 = {
  base00 = term_bg,
  base01 = "{{ colors.surface_container_low.hex }}",
  base02 = lighten("{{ colors.surface_variant.hex }}", 3),
  base03 = lighten("{{ colors.outline.hex }}", 0),
  base04 = term_fg,
  base05 = term_fg,
  base06 = "{{ colors.surface_bright.hex }}",
  base07 = "{{ colors.on_surface.hex }}",
  base08 = "{{ colors.red.hex }}",
  base09 = "{{ colors.yellow.hex }}",
  base0A = "{{ colors.blue.hex }}",
  base0B = "{{ colors.green.hex }}",
  base0C = "{{ colors.cyan.hex }}",
  base0D = lighten("{{ colors.blue.hex }}", 20),
  base0E = "{{ colors.tertiary.hex }}",
  base0F = "{{ colors.inverse_surface.hex }}",
}

M.type = "{{ mode }}"

M.polish_hl = {
  defaults = {
    Comment = {
      italic = true,
      fg = M.base_16.base03,
    },
  },
  Syntax = {
    String = {
      fg = "{{ colors.tertiary.hex }}",
    },
  },
  treesitter = {
    ["@comment"] = {
      fg = M.base_16.base03,
    },
    ["@string"] = {
      fg = "{{ colors.tertiary.hex }}",
    },
  },
}

return M
