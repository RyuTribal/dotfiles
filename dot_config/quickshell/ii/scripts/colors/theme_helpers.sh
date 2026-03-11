#!/usr/bin/env bash

normalize_hex_color() {
    local color="${1:-}"

    color="${color#\#}"
    printf '#%s\n' "${color^^}"
}

sample_image_color() {
    local image_path="$1"
    local magick_cmd=""
    local sampled_color=""

    if command -v magick >/dev/null 2>&1; then
        magick_cmd="magick"
    elif command -v convert >/dev/null 2>&1; then
        magick_cmd="convert"
    else
        echo "[theme_helpers.sh] Error: Neither 'magick' nor 'convert' is available for wallpaper color sampling." >&2
        return 1
    fi

    sampled_color="$("$magick_cmd" "$image_path" -resize 1x1\! -format '#%[hex:p{0,0}]' info: 2>/dev/null)" || return 1
    if [[ "$sampled_color" =~ ^#[0-9A-Fa-f]{8}$ ]]; then
        sampled_color="${sampled_color:0:7}"
    fi

    if [[ ! "$sampled_color" =~ ^#[0-9A-Fa-f]{6}$ ]]; then
        echo "[theme_helpers.sh] Error: Failed to derive a valid hex color from '$image_path'." >&2
        return 1
    fi

    printf '%s\n' "${sampled_color^^}"
}

build_matugen_theme_command() {
    local -n command_ref="$1"
    local image_path="$2"
    local mode_flag="$3"
    local type_flag="$4"
    local explicit_color="$5"
    local theme_color=""

    if [[ -n "$explicit_color" ]]; then
        theme_color="$(normalize_hex_color "$explicit_color")"
    else
        theme_color="$(sample_image_color "$image_path")" || return 1
    fi

    command_ref=(matugen color hex "$theme_color")

    if [[ -n "$mode_flag" ]]; then
        command_ref+=(--mode "$mode_flag")
    fi

    if [[ -n "$type_flag" ]]; then
        command_ref+=(--type "$type_flag")
    fi
}

resolve_material_generator_python() {
    local candidates=()
    local expanded_venv=""
    local candidate=""

    if [[ -n "${ILLOGICAL_IMPULSE_VIRTUAL_ENV:-}" ]]; then
        expanded_venv="$(eval echo "$ILLOGICAL_IMPULSE_VIRTUAL_ENV")"
        candidates+=("$expanded_venv/bin/python3")
    fi
    candidates+=("python3")

    for candidate in "${candidates[@]}"; do
        if [[ "$candidate" == */* ]]; then
            [[ -x "$candidate" ]] || continue
        else
            command -v "$candidate" >/dev/null 2>&1 || continue
        fi

        if "$candidate" -c 'import materialyoucolor, PIL' >/dev/null 2>&1; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done

    return 1
}

find_site_packages_for_module() {
    local module_name="$1"
    local current_site_packages=""
    local candidate=""

    current_site_packages="$(python3 - <<'PY' "$module_name"
import importlib.util
import sys
import os

module_name = sys.argv[1]
spec = importlib.util.find_spec(module_name)
if spec and spec.origin:
    path = spec.origin
    parts = path.split(os.sep)
    if "site-packages" in parts:
        idx = parts.index("site-packages")
        print(os.sep.join(parts[: idx + 1]))
PY
)"
    if [[ -n "$current_site_packages" ]]; then
        printf '%s\n' "$current_site_packages"
        return 0
    fi

    for candidate in /usr/lib/python*/site-packages; do
        [[ -d "$candidate/$module_name" ]] || continue
        printf '%s\n' "$candidate"
        return 0
    done

    return 1
}

build_material_generator_command() {
    local -n command_ref="$1"
    local site_packages=""

    if python3 - <<'PY' >/dev/null 2>&1
import PIL
from materialyoucolor.hct import Hct
PY
    then
        command_ref=(python3)
        return 0
    fi

    site_packages="$(find_site_packages_for_module materialyoucolor)" || return 1
    command_ref=(
        env
        "PYTHONPATH=$site_packages"
        python3
    )
}

build_kde_material_you_colors_command() {
    local -n command_ref="$1"
    local mode_flag="$2"
    local color="$3"
    local scheme_variant="$4"
    local site_packages=""
    local entrypoint_code='import re, sys; from kde_material_you_colors.main import main; sys.argv = ["kde-material-you-colors", *sys.argv[1:]]; sys.argv[0] = re.sub(r"(-script\\.pyw|\\.exe)?$", "", sys.argv[0]); sys.exit(main())'

    site_packages="$(find_site_packages_for_module kde_material_you_colors)" || return 1
    command_ref=(
        env
        "PYTHONPATH=$site_packages"
        python3
        -c
        "$entrypoint_code"
        "$mode_flag"
        --color
        "$color"
        -sv
        "$scheme_variant"
    )
}

write_terminal_palette_json_from_base16() {
    local base16_json_path="$1"
    local output_path="$2"
    local mode="${3:-dark}"
    local output_dir=""
    local tmp_path=""

    output_dir="$(dirname "$output_path")"
    mkdir -p "$output_dir"
    tmp_path="${output_path}.tmp"

    jq \
        --arg mode "$mode" \
        '
        def pick($key):
            .base16[$key][$mode] // .base16[$key].default // .base16[$key].dark // .base16[$key].light;
        def hex($key):
            "#" + (pick($key) | tostring | sub("^#"; "") | ascii_downcase);
        {
            term0: hex("base00"),
            term1: hex("base08"),
            term2: hex("base0b"),
            term3: hex("base0a"),
            term4: hex("base0d"),
            term5: hex("base0e"),
            term6: hex("base0c"),
            term7: hex("base05"),
            term8: hex("base03"),
            term9: hex("base08"),
            term10: hex("base0b"),
            term11: hex("base0a"),
            term12: hex("base0d"),
            term13: hex("base0e"),
            term14: hex("base0c"),
            term15: hex("base07")
        }
        ' \
        "$base16_json_path" > "$tmp_path" || {
        rm -f "$tmp_path"
        return 1
    }

    mv "$tmp_path" "$output_path"
}

render_terminal_sequences_file() {
    local palette_json_path="$1"
    local template_path="$2"
    local output_path="$3"
    local alpha="${4:-100}"
    local output_dir=""
    local tmp_path=""
    local index=""
    local key=""
    local color=""
    local content=""
    local placeholder=""

    output_dir="$(dirname "$output_path")"
    mkdir -p "$output_dir"
    tmp_path="${output_path}.tmp"
    content="$(cat "$template_path")"

    for index in $(seq 0 15); do
        key="term${index}"
        color="$(jq -r --arg key "$key" '.[$key]' "$palette_json_path")"
        if [[ ! "$color" =~ ^#[0-9A-Fa-f]{6}$ ]]; then
            rm -f "$tmp_path"
            echo "[theme_helpers.sh] Error: Missing or invalid $key in $palette_json_path." >&2
            return 1
        fi

        placeholder="\$${key} #"
        content="${content//${placeholder}/${color#\#}}"
    done

    content="${content//\$alpha/$alpha}"
    printf '%s' "$content" > "$tmp_path"
    mv "$tmp_path" "$output_path"
}

write_terminal_palette_json_from_theme() {
    local output_path="$1"
    local image_path="$2"
    local mode_flag="$3"
    local type_flag="$4"
    local explicit_color="$5"
    local termscheme_path="${6:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/terminal/scheme-base.json}"
    local harmony="${7:-0.8}"
    local harmonize_threshold="${8:-100}"
    local term_fg_boost="${9:-0.35}"
    local theme_color=""
    local generator_cmd=()
    local output_dir=""
    local tmp_path=""

    if [[ -n "$explicit_color" ]]; then
        theme_color="$(normalize_hex_color "$explicit_color")"
    else
        theme_color="$(sample_image_color "$image_path")" || return 1
    fi

    build_material_generator_command generator_cmd || return 1
    output_dir="$(dirname "$output_path")"
    mkdir -p "$output_dir"
    tmp_path="${output_path}.tmp"

    if ! "${generator_cmd[@]}" "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/generate_colors_material.py" \
        --color "$theme_color" \
        --mode "$mode_flag" \
        --scheme "$type_flag" \
        --termscheme "$termscheme_path" \
        --blend_bg_fg \
        --harmony "$harmony" \
        --harmonize_threshold "$harmonize_threshold" \
        --term_fg_boost "$term_fg_boost" \
        --terminal-json > "$tmp_path"; then
        rm -f "$tmp_path"
        return 1
    fi

    mv "$tmp_path" "$output_path"
}
