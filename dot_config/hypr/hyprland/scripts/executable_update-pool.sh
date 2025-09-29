#!/usr/bin/env bash
# ~/.config/hypr/hyprland/scripts/update-pool.sh
# Detects installed package managers and prints a merged JSON:
# {
#   "managers": {
#     "pacman": { "name":"Pacman", "packages":[{"name":"pkg","cur":"1.0-1","new":"1.1-1"}] },
#     "aur":    { "name":"AUR",    "packages":[{"name":"foo-git","cur":"r100","new":"r120"}] },
#     "flatpak":{ "name":"Flatpak","packages":[...] },
#     "snap":   { "name":"Snap",   "packages":[...] },
#     "pip":    { "name":"pip",    "packages":[...] },
#     "npm":    { "name":"npm -g", "packages":[...] },
#     "brew":   { "name":"Homebrew","packages":[...] },
#     "fwupd":  { "name":"fwupd",  "packages":[...] },
#     "rustup": { "name":"rustup toolchains","packages":[...] }
#   },
#   "counts": { "repo":N, "aur":M }
# }

set -euo pipefail

JQ=${JQ:-jq}
if ! command -v "${JQ}" >/dev/null 2>&1; then
  echo '{"managers":{},"counts":{"repo":0,"aur":0}}'
  exit 0
fi

json_escape() { "${JQ}" -Rr @json; }

declare -A managers_json
declare -A counts
counts[repo]=0
counts[aur]=0

############################################
# pacman (repos) via checkupdates
############################################
if command -v checkupdates >/dev/null 2>&1; then
  pacman_pkgs=$(
    checkupdates 2>/dev/null | awk '
      NF>=4 && $3=="->" {
        name=$1; cur=$2; new=$4;
        printf("{\"name\":%s,\"cur\":%s,\"new\":%s}\n",
          tojson(name), tojson(cur), tojson(new))
      }
      function tojson(s){ gsub(/"/,"\\\"",s); return "\"" s "\"" }
    ' | paste -sd, -
  )
  [[ -n "$pacman_pkgs" ]] || pacman_pkgs=""
  pacman_pkgs="[${pacman_pkgs}]"
  counts[repo]=$(${JQ} -r 'length' <<<"$pacman_pkgs")
  managers_json[pacman]=$(${JQ} -c --arg name "Pacman" --argjson pkgs "$pacman_pkgs" -n '{name:$name,packages:$pkgs}')
fi

############################################
# AUR via yay/paru -Qua
############################################
aur_list=""
if command -v yay >/dev/null 2>&1; then
  aur_list="$(yay -Qua 2>/dev/null || true)"
elif command -v paru >/dev/null 2>&1; then
  aur_list="$(paru -Qua 2>/dev/null || true)"
fi
if [[ -n "$aur_list" ]]; then
  aur_pkgs=$(
    awk '
      NF>=4 && $3=="->" {
        name=$1; cur=$2; new=$4;
        printf("{\"name\":%s,\"cur\":%s,\"new\":%s}\n",
          tojson(name), tojson(cur), tojson(new))
      }
      function tojson(s){ gsub(/"/,"\\\"",s); return "\"" s "\"" }
    ' <<<"$aur_list" | paste -sd, -
  )
  [[ -n "$aur_pkgs" ]] || aur_pkgs=""
  aur_pkgs="[${aur_pkgs}]"
  counts[aur]=$(${JQ} -r 'length' <<<"$aur_pkgs")
  managers_json[aur]=$(${JQ} -c --arg name "AUR" --argjson pkgs "$aur_pkgs" -n '{name:$name,packages:$pkgs}')
fi

############################################
# Flatpak (commit-based; version often empty)
############################################
if command -v flatpak >/dev/null 2>&1; then
  refs="$(flatpak remote-ls --updates 2>/dev/null || true)"
  if [[ -n "$refs" ]]; then
    flatpak_rows=()
    while IFS= read -r ref; do
      [[ -z "$ref" ]] && continue
      cur_commit="$(flatpak info "$ref" --show-commit 2>/dev/null || true)"
      origin="$(flatpak info "$ref" --show-origin 2>/dev/null || true)"
      new_commit=""
      if [[ -n "$origin" ]]; then
        new_commit="$(flatpak remote-info "$origin" "$ref" --log 2>/dev/null | awk 'NR==1{print $1; exit}')"
      fi
      cur_ver="$(flatpak info "$ref" 2>/dev/null | awk -F': *' '/^Version:/ {print $2; exit}')"
      cur="${cur_ver:-$cur_commit}"
      new="${new_commit:-""}"
      flatpak_rows+=("{\"name\":$(printf '%s' "$ref" | json_escape),\"cur\":$(printf '%s' "$cur" | json_escape),\"new\":$(printf '%s' "$new" | json_escape)}")
    done <<<"$refs"
    if ((${#flatpak_rows[@]})); then
      flatpak_json="[$(
        IFS=,
        echo "${flatpak_rows[*]}"
      )]"
      managers_json[flatpak]=$(${JQ} -c --arg name "Flatpak" --argjson pkgs "$flatpak_json" -n '{name:$name,packages:$pkgs}')
    fi
  fi
fi

############################################
# Snap
############################################
if command -v snap >/dev/null 2>&1; then
  snap_out="$(snap refresh --list 2>/dev/null || true)"
  # Parse conservatively: use the first three whitespace-delimited columns after the header.
  # Different snapd versions change columns; this yields Name, Installed, Available in many builds.
  if [[ -n "$snap_out" ]]; then
    snap_pkgs=$(
      awk 'NR>1 && NF>=3 { printf("{\"name\":%s,\"cur\":%s,\"new\":%s}\n", tojson($1), tojson($2), tojson($3)) }
           function tojson(s){ gsub(/"/,"\\\"",s); return "\"" s "\"" }' <<<"$snap_out" |
        paste -sd, -
    )
    [[ -n "$snap_pkgs" ]] && snap_pkgs="[$snap_pkgs]" || snap_pkgs="[]"
    managers_json[snap]=$(${JQ} -c --arg name "Snap" --argjson pkgs "$snap_pkgs" -n '{name:$name,packages:$pkgs}')
  fi
fi

############################################
# Assemble final JSON
############################################
printf '{'
printf '"managers":{'
first=1
for k in "${!managers_json[@]}"; do
  [[ $first -eq 0 ]] && printf ','
  first=0
  printf '%s:%s' "$(printf '%s' "$k" | ${JQ} -Rr @json)" "${managers_json[$k]}"
done
printf '},'
printf '"counts":{"repo":%s,"aur":%s}' "${counts[repo]}" "${counts[aur]}"
printf '}\n'
