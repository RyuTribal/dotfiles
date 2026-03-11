#!/usr/bin/env bash

QUICKSHELL_CONFIG_NAME="ii"
XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
XDG_CACHE_HOME="${XDG_CACHE_HOME:-$HOME/.cache}"
XDG_STATE_HOME="${XDG_STATE_HOME:-$HOME/.local/state}"
CONFIG_DIR="$XDG_CONFIG_HOME/quickshell/$QUICKSHELL_CONFIG_NAME"
CACHE_DIR="$XDG_CACHE_HOME/quickshell"
STATE_DIR="$XDG_STATE_HOME/quickshell"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

term_alpha=100 #Set this to < 100 make all your terminals transparent
terminal_palette_path="$STATE_DIR/user/generated/terminal_palette.json"
terminal_sequences_path="$STATE_DIR/user/generated/terminal/sequences.txt"
# sleep 0 # idk i wanted some delay or colors dont get applied properly
if [ ! -d "$STATE_DIR"/user/generated ]; then
  mkdir -p "$STATE_DIR"/user/generated
fi
cd "$CONFIG_DIR" || exit

source "$SCRIPT_DIR/theme_helpers.sh"

apply_term() {
  # Check if terminal escape sequence template exists
  if [ ! -f "$SCRIPT_DIR/terminal/sequences.txt" ]; then
    echo "Template file not found for Terminal. Skipping that."
    return
  fi

  if [ -f "$terminal_palette_path" ]; then
    render_terminal_sequences_file "$terminal_palette_path" "$SCRIPT_DIR/terminal/sequences.txt" "$terminal_sequences_path" "$term_alpha"
  elif [ -f "$STATE_DIR/user/generated/material_colors.scss" ]; then
    echo "[applycolor.sh] Warning: Falling back to legacy material_colors.scss terminal palette." >&2
    local colornames=''
    local colorstrings=''
    local colorlist=()
    local colorvalues=()
    local i=0

    colornames=$(cat "$STATE_DIR/user/generated/material_colors.scss" | cut -d: -f1)
    colorstrings=$(cat "$STATE_DIR/user/generated/material_colors.scss" | cut -d: -f2 | cut -d ' ' -f2 | cut -d ";" -f1)
    IFS=$'\n'
    colorlist=($colornames)
    colorvalues=($colorstrings)
    unset IFS

    mkdir -p "$STATE_DIR"/user/generated/terminal
    cp "$SCRIPT_DIR/terminal/sequences.txt" "$terminal_sequences_path"
    for i in "${!colorlist[@]}"; do
      sed -i "s/${colorlist[$i]} #/${colorvalues[$i]#\#}/g" "$terminal_sequences_path"
    done
    sed -i "s/\$alpha/$term_alpha/g" "$terminal_sequences_path"
  else
    echo "[applycolor.sh] Warning: No terminal palette source found. Skipping terminal theming." >&2
    return
  fi

  for file in /dev/pts/*; do
    if [[ $file =~ ^/dev/pts/[0-9]+$ ]]; then
      {
      cat "$terminal_sequences_path" >"$file"
      } & disown || true
    fi
  done
}

apply_qt() {
  sh "$CONFIG_DIR/scripts/kvantum/materialQT.sh"          # generate kvantum theme
  python "$CONFIG_DIR/scripts/kvantum/changeAdwColors.py" # apply config colors
}

# Check if terminal theming is enabled in config
CONFIG_FILE="$XDG_CONFIG_HOME/illogical-impulse/config.json"
if [ -f "$CONFIG_FILE" ]; then
  enable_terminal=$(jq -r '.appearance.wallpaperTheming.enableTerminal' "$CONFIG_FILE")
  if [ "$enable_terminal" = "true" ]; then
    apply_term &
  fi
else
  echo "Config file not found at $CONFIG_FILE. Applying terminal theming by default."
  apply_term &
fi

# apply_qt & # Qt theming is already handled by kde-material-colors
