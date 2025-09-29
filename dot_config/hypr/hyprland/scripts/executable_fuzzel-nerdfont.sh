#!/usr/bin/env bash
set -euo pipefail

MODE="copy"
UPDATE=false
DUMP=false
FONT_RE="Nerd|NFP|NFM"
while (("$#")); do
  case "$1" in
  type | copy | both)
    MODE="$1"
    shift
    ;;
  --update)
    UPDATE=true
    shift
    ;;
  --dump)
    DUMP=true
    shift
    ;;
  --font)
    FONT_RE="${2:-Nerd}"
    shift 2
    ;;
  *)
    echo "Usage: $0 [type|copy|both] [--update] [--dump] [--font <regex>]"
    exit 1
    ;;
  esac
done

XDG_CACHE_HOME="${XDG_CACHE_HOME:-$HOME/.cache}"
CACHE_DIR="$XDG_CACHE_HOME/nerdfonts"
META_JSON="$CACHE_DIR/glyphnames.json"
LIST_TXT="$CACHE_DIR/local-glyphs.txt"
FONTS_SIG="$CACHE_DIR/fonts.sig"
JSON_URL="https://raw.githubusercontent.com/ryanoasis/nerd-fonts/HEAD/glyphnames.json"

mkdir -p "$CACHE_DIR"

# 1) Download metadata only if newer (or first time / --update)
if [[ "$UPDATE" == "true" || ! -s "$META_JSON" ]]; then
  curl -fsSL "$JSON_URL" -o "$META_JSON"
else
  # conditional GET: only writes if remote is newer, preserves time
  curl -fsSL -z "$META_JSON" "$JSON_URL" -o "$META_JSON"
fi
# (curl --time-cond / -z: only transfer if remote is newer. )  :contentReference[oaicite:0]{index=0}

# 2) Gather installed Nerd Font files
mapfile -t NF_FILES < <(fc-list : file family | awk -v re="$FONT_RE" 'BEGIN{FS=":"} $0~re {gsub(/^ +| +$/,"",$1); print $1}' | sort -u)
((${#NF_FILES[@]})) || {
  echo "No installed Nerd Fonts matched regex: /$FONT_RE/" >&2
  exit 1
}

# 3) Build a signature of the font set; rebuild list only if fonts changed or list missing
new_sig="$(printf '%s\n' "${NF_FILES[@]}" | sha256sum | awk '{print $1}')"
old_sig="$(cat "$FONTS_SIG" 2>/dev/null || true)"

rebuild=false
[[ ! -s "$LIST_TXT" ]] && rebuild=true
[[ "$new_sig" != "$old_sig" ]] && rebuild=true
# also rebuild if metadata changed later than the list
[[ "$META_JSON" -nt "$LIST_TXT" ]] && rebuild=true

if $rebuild; then
  command -v python3 >/dev/null 2>&1 || {
    echo "python3 required" >&2
    exit 1
  }
  python3 - "$META_JSON" "${NF_FILES[@]}" >"$LIST_TXT".tmp <<'PY'
import sys, json
from fontTools.ttLib import TTFont

meta = json.load(open(sys.argv[1], "r", encoding="utf-8"))
paths = sys.argv[2:]

# tolerate both { "glyphs": {...} } and flat { "nf-*": {...} }
entries = meta.get("glyphs", meta)
codes = set()
for p in paths:
    try:
        tt = TTFont(p, fontNumber=0, lazy=True)
        for t in tt["cmap"].tables:
            if t.cmap: codes.update(t.cmap.keys())
        tt.close()
    except Exception:
        pass

for nf_id, v in entries.items():
    code_hex = v.get("code") or v.get("codepoint") or v.get("unicode")
    if not code_hex: continue
    try: cp = int(code_hex, 16)
    except Exception: continue
    if cp not in codes: continue
    glyph = chr(cp)
    name = (v.get("name","") or "").replace("-", " ").strip()
    kw   = v.get("search", [])
    if isinstance(kw, list): kw = " ".join(kw)
    else: kw = str(kw)
    out = [glyph]
    if name: out.append(name)
    if kw:   out.append(kw)
    out.append(nf_id)
    print(" ".join(out))
PY
  mv "$LIST_TXT".tmp "$LIST_TXT"
  printf '%s\n' "$new_sig" >"$FONTS_SIG"
fi

if [[ "$DUMP" == "true" ]]; then
  cat "$LIST_TXT"
  exit 0
fi

sel="$(cat "$LIST_TXT" | fuzzel --match-mode fzf --dmenu || true)"
glyph="$(printf '%s' "$sel" | awk '{print $1}')"
[[ -z "$glyph" ]] && exit 0

case "$MODE" in
type) wtype "$glyph" || wl-copy "$glyph" ;;
copy) wl-copy "$glyph" ;;
both)
  wtype "$glyph" || true
  wl-copy "$glyph"
  ;;
esac
