#!/usr/bin/env -S\_/bin/sh\_-c\_"source\_\$(eval\_echo\_\$ILLOGICAL_IMPULSE_VIRTUAL_ENV)/bin/activate&&exec\_python\_-E\_"\$0"\_"\$@""
import argparse
import os
import re
from typing import Dict, List, Optional, Tuple

# Reads Hyprland 0.55+ Lua keybind files (hl.bind) and emits the same JSON shape
# the hyprlang version of this script produced, so the cheatsheet is unchanged.
#
# Conventions carried over from the .conf format:
#   --!        section heading, depth 1     (was #!)
#   --#!       section heading, depth 2     (was ##!)
#   --/#       pseudo-bind, shown but not real (was #/#)
#   -- [hidden] trailing comment suppresses the bind
# Depth is (number of '#' before the '!') + 1, because converting `#!` to a Lua
# comment consumed the first '#'.

HEADING_REGEX = re.compile(r"^--(#*)!")
PSEUDO_BIND_PREFIX = "--/#"
HIDE_COMMENT = "[hidden]"

parser = argparse.ArgumentParser(description='Hyprland keybind reader (Lua config)')
parser.add_argument('--path', type=str, default="$HOME/.config/hypr/hyprland.lua",
                    help='path to keybind file (require() isn\'t followed)')
args = parser.parse_args()


class KeyBinding(dict):
    def __init__(self, mods, key, dispatcher, params, comment) -> None:
        self["mods"] = mods
        self["key"] = key
        self["dispatcher"] = dispatcher
        self["params"] = params
        self["comment"] = comment


class Section(dict):
    def __init__(self, children, keybinds, name) -> None:
        self["children"] = children
        self["keybinds"] = keybinds
        self["name"] = name


def read_content(path: str) -> str:
    resolved = os.path.expanduser(os.path.expandvars(path))
    if not os.access(resolved, os.R_OK):
        return "error"
    with open(resolved, "r") as file:
        return file.read()


def split_code_and_comment(line: str) -> Tuple[str, Optional[str]]:
    """Split a Lua line into code and its trailing `--` comment.

    Scans character by character because exec_cmd strings legitimately contain
    `--` (`--fullscreen`, `--match-mode fzf`), which a naive split would eat.
    """
    in_string = False
    i = 0
    while i < len(line):
        ch = line[i]
        if in_string:
            if ch == "\\":
                i += 2
                continue
            if ch == '"':
                in_string = False
        elif ch == '"':
            in_string = True
        elif ch == "-" and line[i + 1:i + 2] == "-":
            return line[:i], line[i + 2:].strip()
        i += 1
    return line, None


def split_args(text: str) -> List[str]:
    """Split a Lua argument list on top-level commas, respecting strings/nesting."""
    args_out, depth, in_string, start = [], 0, False, 0
    i = 0
    while i < len(text):
        ch = text[i]
        if in_string:
            if ch == "\\":
                i += 2
                continue
            if ch == '"':
                in_string = False
        elif ch == '"':
            in_string = True
        elif ch in "({[":
            depth += 1
        elif ch in ")}]":
            depth -= 1
        elif ch == "," and depth == 0:
            args_out.append(text[start:i].strip())
            start = i + 1
        i += 1
    tail = text[start:].strip()
    if tail:
        args_out.append(tail)
    return args_out


def bind_call_body(code: str) -> Optional[str]:
    """Return the text inside `hl.bind( ... )`, or None if this isn't a bind."""
    m = re.search(r"\bhl\.bind\s*\(", code)
    if not m:
        return None
    depth, in_string, start = 1, False, m.end()
    i = start
    while i < len(code):
        ch = code[i]
        if in_string:
            if ch == "\\":
                i += 2
                continue
            if ch == '"':
                in_string = False
        elif ch == '"':
            in_string = True
        elif ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
            if depth == 0:
                return code[start:i]
        i += 1
    return None


def parse_keys_string(keys: str) -> Tuple[List[str], str]:
    parts = [p.strip() for p in keys.split("+") if p.strip()]
    if not parts:
        return [], ""
    return parts[:-1], parts[-1]


# hl.dsp.* (and the local workspace helpers) mapped back to the legacy dispatcher
# names, so autogenerate_comment() still has something to work with when a bind
# carries no comment of its own.
DISPATCHER_MAP = {
    "hl.dsp.exec_cmd": "exec",
    "hl.dsp.exec_raw": "exec",
    "hl.dsp.global": "global",
    "hl.dsp.layout": "layoutmsg",
    "hl.dsp.submap": "submap",
    "hl.dsp.window.close": "killactive",
    "hl.dsp.window.kill": "killactive",
    "hl.dsp.window.float": "togglefloating",
    "hl.dsp.window.fullscreen": "fullscreen",
    "hl.dsp.window.pin": "pin",
    "hl.dsp.window.center": "centerwindow",
    "hl.dsp.window.drag": "movewindow",
    "hl.dsp.window.resize": "resizewindow",
    "hl.dsp.window.move": "movetoworkspace",
    "hl.dsp.window.swap": "swapwindow",
    "hl.dsp.focus": "movefocus",
    "hl.dsp.workspace.toggle_special": "togglespecialworkspace",
    "hl.dsp.workspace.move": "movecurrentworkspacetomonitor",
    "focus_workspace_in_group": "workspace",
    "move_to_workspace_in_group": "movetoworkspace",
}


def parse_dispatcher(expr: str) -> Tuple[str, str]:
    """Best-effort (dispatcher, params) for display purposes."""
    m = re.match(r"([A-Za-z_][\w.]*)\s*\(", expr)
    if not m:
        return ("lua", "")
    callee = m.group(1)
    inner = expr[m.end():].rsplit(")", 1)[0].strip()
    first = split_args(inner)[0] if split_args(inner) else ""
    if first.startswith('"') and first.endswith('"') and len(first) >= 2:
        first = first[1:-1]
    return (DISPATCHER_MAP.get(callee, callee), first)


def opts_description(args_list: List[str]) -> Optional[str]:
    """Pull `description`/`desc` out of the options table, if present."""
    if len(args_list) < 3:
        return None
    m = re.search(r"\b(?:description|desc)\s*=\s*\"((?:[^\"\\]|\\.)*)\"", args_list[2])
    return m.group(1).replace('\\"', '"') if m else None


def autogenerate_comment(dispatcher: str, params: str = "") -> str:
    direction = {"l": "left", "r": "right", "u": "up", "d": "down"}
    match dispatcher:
        case "resizewindow":
            return "Resize window"
        case "movewindow":
            return "Move window" if params == "" else "Window: move in {} direction".format(direction.get(params, "null"))
        case "pin":
            return "Window: pin (show on all workspaces)"
        case "splitratio":
            return "Window split ratio {}".format(params)
        case "togglefloating":
            return "Float/unfloat window"
        case "resizeactive":
            return "Resize window by {}".format(params)
        case "killactive":
            return "Close window"
        case "centerwindow":
            return "Center window"
        case "fullscreen":
            return "Toggle {}".format({
                "0": "fullscreen",
                "1": "maximization",
                "2": "fullscreen on Hyprland's side",
            }.get(params, "fullscreen"))
        case "workspace":
            if params == "+1":
                return "Workspace: focus right"
            if params == "-1":
                return "Workspace: focus left"
            return "Focus workspace {}".format(params)
        case "movefocus":
            return "Window: move focus {}".format(direction.get(params, "null"))
        case "swapwindow":
            return "Window: swap in {} direction".format(direction.get(params, "null"))
        case "movetoworkspace":
            if params == "+1":
                return "Window: move to right workspace (non-silent)"
            if params == "-1":
                return "Window: move to left workspace (non-silent)"
            return "Window: move to workspace {} (non-silent)".format(params)
        case "movetoworkspacesilent":
            return "Window: move to workspace {}".format(params)
        case "togglespecialworkspace":
            return "Workspace: toggle special"
        case "movecurrentworkspacetomonitor":
            return "Workspace: move to monitor {}".format(params)
        case "exec":
            return "Execute: {}".format(params)
        case "global":
            return ""
        case _:
            return ""


def build_keybind(code: str, comment: Optional[str]) -> Optional[KeyBinding]:
    body = bind_call_body(code)
    if body is None:
        return None
    args_list = split_args(body)
    if not args_list:
        return None

    keys = args_list[0].strip()
    if keys.startswith('"') and keys.endswith('"'):
        keys = keys[1:-1]
    mods, key = parse_keys_string(keys)

    dispatcher, params = parse_dispatcher(args_list[1]) if len(args_list) > 1 else ("lua", "")

    # trailing comment wins, then the description field, then a generated string
    if comment:
        if comment.startswith(HIDE_COMMENT):
            return None
        text = comment
    else:
        text = opts_description(args_list) or autogenerate_comment(dispatcher, params)

    return KeyBinding(mods, key, dispatcher, params, text)


def parse_keys(path: str) -> Dict[str, List[KeyBinding]]:
    content = read_content(path)
    if content == "error":
        return "error"

    root = Section([], [], "")
    stack: List[Tuple[int, Section]] = [(0, root)]

    for raw in content.splitlines():
        line = raw.strip()

        heading = HEADING_REGEX.match(line)
        if heading:
            depth = len(heading.group(1)) + 1
            name = line[heading.end():].strip()
            while len(stack) > 1 and stack[-1][0] >= depth:
                stack.pop()
            section = Section([], [], name)
            stack[-1][1]["children"].append(section)
            stack.append((depth, section))
            continue

        if line.startswith(PSEUDO_BIND_PREFIX):
            # `--/# bind = SUPER, ←/↑/→/↓,, # Focus in direction` -- documentation-only
            rest = line[len(PSEUDO_BIND_PREFIX):].strip()
            payload, comment = split_code_and_comment(rest)
            payload, _, hyprlang_comment = payload.partition("#")
            comment = comment or hyprlang_comment.strip() or None
            if comment and comment.startswith(HIDE_COMMENT):
                continue
            # still hyprlang shaped: `bind = <mods>, <key>, <dispatcher>, <params>`
            # -- the mods are the whole first field, the key is the second.
            parts = [p.strip() for p in payload.split("=", 1)[-1].split(",")]
            mods = [m.strip() for m in parts[0].split("+") if m.strip()] if parts else []
            key = parts[1] if len(parts) > 1 else ""
            stack[-1][1]["keybinds"].append(
                KeyBinding(mods, key, "", "", comment or "")
            )
            continue

        code, comment = split_code_and_comment(line)
        if "hl.bind" not in code:
            continue
        keybind = build_keybind(code, comment)
        if keybind is not None:
            stack[-1][1]["keybinds"].append(keybind)

    return root


if __name__ == "__main__":
    import json

    ParsedKeys = parse_keys(args.path)
    print(json.dumps(ParsedKeys))
