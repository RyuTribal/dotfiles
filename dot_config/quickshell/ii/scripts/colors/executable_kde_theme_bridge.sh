#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/theme_helpers.sh"

XDG_STATE_HOME="${XDG_STATE_HOME:-$HOME/.local/state}"

color="$(tr -d '\n' < "$XDG_STATE_HOME/quickshell/user/generated/color.txt")"

current_mode="$(gsettings get org.gnome.desktop.interface color-scheme 2>/dev/null | tr -d "'")"
if [[ "$current_mode" == "prefer-dark" ]]; then
    mode_flag="-d"
else
    mode_flag="-l"
fi

scheme_variant_str=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --scheme-variant)
            scheme_variant_str="$2"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done

case "$scheme_variant_str" in
    scheme-content) sv_num=0 ;;
    scheme-expressive) sv_num=1 ;;
    scheme-fidelity) sv_num=2 ;;
    scheme-monochrome) sv_num=3 ;;
    scheme-neutral) sv_num=4 ;;
    scheme-tonal-spot) sv_num=5 ;;
    scheme-vibrant) sv_num=6 ;;
    scheme-rainbow) sv_num=7 ;;
    scheme-fruit-salad) sv_num=8 ;;
    "") sv_num=5 ;;
    *)
        echo "Unknown scheme variant: $scheme_variant_str" >&2
        exit 1
        ;;
esac

declare -a cmd=()
build_kde_material_you_colors_command cmd "$mode_flag" "$color" "$sv_num"
"${cmd[@]}"
