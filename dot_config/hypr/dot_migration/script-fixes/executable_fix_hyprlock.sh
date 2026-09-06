#!/bin/bash
pkill -9 hyprlock

hyprctl --instance 0 'keyword misc:allow_session_lock_restore 1'
# Lua config: `dispatch exec hyprlock` is rejected, dispatch args are read as Lua.
hyprctl --instance 0 'dispatch hl.dsp.exec_cmd("hyprlock")'
