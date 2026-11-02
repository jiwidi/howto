# HowTo shell integration for fish.
# On an empty prompt, Tab inserts the pending command without submitting it.

status is-interactive; or return
set -q _HOWTO_SHELL_INTEGRATION_LOADED; and return
set -g _HOWTO_SHELL_INTEGRATION_LOADED 1

set -l howto_resolved_executable (command -s howto 2>/dev/null); or return
test (count $howto_resolved_executable) -eq 1; or return
string match -qr '^/[^\r\n]*$' -- "$howto_resolved_executable"; or return
test -x "$howto_resolved_executable"; or return
set -l _howto_executable "$howto_resolved_executable"

set -gx HOWTO_SHELL_SESSION "fish-$fish_pid-"(random)"-"(random)

# Bind every variable that can select HowTo state, model, or runtime before a
# project-local environment manager can replace it. Config files remain live:
# each automatic call still reloads the original config path.
set -l _howto_environment "HOWTO_SHELL_SESSION=$HOWTO_SHELL_SESSION"
for howto_environment_name in \
        HOME HOWTO_HOME \
        XDG_CONFIG_HOME XDG_DATA_HOME XDG_CACHE_HOME XDG_STATE_HOME XDG_RUNTIME_DIR \
        TMPDIR TMP TEMP PATH \
        HOWTO_MODEL HOWTO_PACKAGED_MODEL HOWTO_LLAMA_SERVER \
        LANG LC_ALL LC_CTYPE \
        CUDA_VISIBLE_DEVICES HIP_VISIBLE_DEVICES ROCR_VISIBLE_DEVICES
    if set -q -x $howto_environment_name
        set -a _howto_environment \
            "$howto_environment_name="(string join : $$howto_environment_name)
    end
end
set -l _howto_env_executable /usr/bin/env

function __howto_failed_command_advisor --on-event fish_postexec \
        --inherit-variable _howto_executable \
        --inherit-variable _howto_env_executable \
        --inherit-variable _howto_environment
    set -l failed_status $status

    test -n "$fish_private_mode"; and return 0
    set -q __howto_advisor_running; and return 0
    if test $failed_status -eq 0; or test $failed_status -ge 128
        return 0
    end
    command "$_howto_env_executable" -i $_howto_environment \
        "$_howto_executable" shell advisor-ready </dev/null >/dev/null 2>&1; or return 0

    set -l command_line "$argv[1]"
    test -n "$command_line"; or return 0
    string match -qr '^[[:space:]]' -- "$command_line"; and return 0
    string match -q -- "*"\n"*" "$command_line"; and return 0
    string match -q -- "*"\r"*" "$command_line"; and return 0
    string match -qr '^(command[[:space:]]+)?([^[:space:]]*/)?howto([[:space:]]|$)' -- "$command_line"; and return 0
    string match -qr '^(exit|logout|return)([[:space:]]|$)' -- "$command_line"; and return 0

    set -g __howto_advisor_running 1
    printf %s "$command_line" |
        command "$_howto_env_executable" -i $_howto_environment \
            "$_howto_executable" shell advise --status "$failed_status"
    set -e __howto_advisor_running
    return 0
end

function __howto_tab \
        --inherit-variable _howto_executable \
        --inherit-variable _howto_env_executable \
        --inherit-variable _howto_environment
    set -l current (commandline)
    if test -z "$current"
        set -l pending (command "$_howto_env_executable" -i $_howto_environment \
            "$_howto_executable" shell take 2>/dev/null)
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
