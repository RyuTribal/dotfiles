# ~/.config/fish/conf.d/fastfetch.fish

# Only in interactive terminals
if status is-interactive

    # Skip if the binary isn't present
    if command -q fastfetch

        # Avoid noisy environments
        if test -z "$CI"; and test "$TERM" != "dumb"

            # Optional: point to a custom config file if you have one
            # set -x FASTFETCH_CONFIG ~/.config/fastfetch/config.jsonc

            set ff_args
            if set -q FASTFETCH_CONFIG
                set ff_args --config $FASTFETCH_CONFIG
            end

            # Optional: skip on SSH; comment this block out if you DO want it on SSH
            if set -q SSH_TTY
                # comment the next line to allow on SSH
                return
            end

            # Run it
            fastfetch $ff_args
        end
    end
end
