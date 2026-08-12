# HowTo shell integration for fish.
# On an empty prompt, Tab inserts the pending command without submitting it.

status is-interactive; or return
set -q _HOWTO_SHELL_INTEGRATION_LOADED; and return
set -g _HOWTO_SHELL_INTEGRATION_LOADED 1

set -gx HOWTO_SHELL_SESSION "fish-$fish_pid-"(random)"-"(random)

function __howto_tab
    set -l current (commandline)
    if test -z "$current"
        set -l pending (command howto shell take 2>/dev/null)
        set -l take_status $status
        if test $take_status -eq 0
            and test (count $pending) -eq 1
            and test -n "$pending[1]"
            and not string match -q -- "*"\r"*" "$pending[1]"
            and not string match -q -- "*"\t"*" "$pending[1]"
            and not string match -q -- "*"\e"*" "$pending[1]"
            commandline --replace -- "$pending[1]"
            commandline -f end-of-line repaint
            return
        end
    end

    commandline -f complete
end

bind \t __howto_tab
bind -M insert \t __howto_tab
