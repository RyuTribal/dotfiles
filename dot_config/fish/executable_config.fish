fish_config theme choose axon-ansi
function fish_prompt -d "Write out the prompt"
    # This shows up as USER@HOST /home/user/ >, with the directory colored
    # $USER and $hostname are set by fish, so you can just use them
    # instead of using `whoami` and `hostname`
    printf '%s@%s %s%s%s > ' $USER $hostname \
        (set_color $fish_color_cwd) (prompt_pwd) (set_color normal)
end

set -x SYSTEMD_EDITOR nvim


if status is-interactive # Commands to run in interactive sessions can go here

    # No greeting
    set fish_greeting

    # Use starship
    starship init fish | source

    # Aliases
    # --icons takes an optional WHEN, so bare `--icons` swallows the next arg:
    # `ls .config` became `eza --icons .config`. Attach the value with `=`.
    alias ls 'eza --icons=auto'
    alias clear "printf '\033[2J\033[3J\033[1;1H'"
    alias q 'qs -c ii'
    alias kssh 'kitten ssh'
    
end


# Added by Antigravity CLI installer
set -gx PATH "/home/ryutribal/.local/bin" $PATH
