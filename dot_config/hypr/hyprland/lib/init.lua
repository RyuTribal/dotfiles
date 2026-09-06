-- Replaces hyprland/scripts/workspace_action.sh.
--
-- Under a Lua config `hyprctl dispatch <legacy args>` is rejected, so the old
-- script's `hyprctl dispatch workspace N` calls no longer work. The arithmetic
-- it did is reproduced here and the dispatch happens in-process -- no subprocess
-- per keypress, and nothing depends on hyprctl's argument syntax.

workspaceGroupSize = 10

-- Map 1..10 onto the current group of `workspaceGroupSize` workspaces:
-- on workspace 14, workspace_in_group(3) -> 13.
-- Mirrors the old shell math: ((curr - 1) / 10) * 10 + i
function workspace_in_group(i)
    local curr = hl.get_active_workspace().id
    return math.floor((curr - 1) / workspaceGroupSize) * workspaceGroupSize + i
end

-- Focus workspace `i` within the current group.
function focus_workspace_in_group(i)
    return function()
        hl.dispatch(hl.dsp.focus({ workspace = workspace_in_group(i) }))
    end
end

-- Move the active window to workspace `i` within the current group and follow it,
-- matching the old `movetoworkspace` dispatcher.
function move_to_workspace_in_group(i)
    return function()
        hl.dispatch(hl.dsp.window.move({ workspace = workspace_in_group(i), follow = true }))
    end
end
