function __howto_management_is
    set -l target $argv[1]
    set -l words (commandline -opc)
    test (count $words) -ge 2; or return 1
    test "$words[2]" = "$target"; or return 1

    switch $target
        case help version
            test (count $words) -eq 2
        case doctor
            test (count $words) -eq 2; and return 0
            string match -q -- '-*' $words[3]
        case setup
            test (count $words) -eq 2; and return 0
            string match -q -- '-*' $words[3]
        case model
            test (count $words) -eq 2; and return 0
            contains -- $words[3] status install; or string match -q -- '-*' $words[3]
        case server
            test (count $words) -eq 2; and return 0
            contains -- $words[3] status stop; or string match -q -- '-*' $words[3]
        case shell
            test (count $words) -ge 3; or return 1
            contains -- $words[3] init enable disable status; or string match -q -- '-*' $words[3]
        case config
            test (count $words) -eq 2; and return 0
            contains -- $words[3] list path get set unset; or string match -q -- '-*' $words[3]
        case '*'
            return 1
    end
end

function __howto_first_argument
    set -l words (commandline -opc)
    test (count $words) -eq 1
end

function __howto_management_root_is
    set -l words (commandline -opc)
    test (count $words) -eq 2; and test "$words[2]" = "$argv[1]"
end

function __howto_management_action_is
    set -l words (commandline -opc)
    test (count $words) -ge 3; or return 1
    test "$words[2]" = "$argv[1]"; and test "$words[3]" = "$argv[2]"
end

function __howto_management_option_led
    set -l words (commandline -opc)
    test (count $words) -ge 3; or return 1
    test "$words[2]" = "$argv[1]"; and string match -q -- '-*' $words[3]
end

function __howto_config_key_context
    set -l words (commandline -opc)
    test (count $words) -eq 3; or return 1
    test "$words[2]" = config; and contains -- $words[3] get set unset
end

function __howto_show_tab_hint_value_context
    set -l words (commandline -opc)
    test (count $words) -eq 4; or return 1
    test "$words[2]" = config
    and test "$words[3]" = set
    and test "$words[4]" = show_tab_hint
end

function __howto_query_option_context
    for target in setup doctor model server shell config help version
        __howto_management_is $target; and return 1
    end

    set -l words (commandline -opc)
    set -l index 2
    while test $index -le (count $words)
        switch $words[$index]
            case -x -e --execute -c --copy -q --quiet --json --timing
                set index (math $index + 1)
            case -n --count
                set index (math $index + 2)
            case --
                return 1
            case '-*'
                return 1
            case '*'
                return 1
        end
    end
    return 0
end

function __howto_help_context
    __howto_query_option_context; and return 0
    for target in setup doctor model server shell config
        __howto_management_is $target; and return 0
    end
    return 1
end

for command in howto
    complete -c $command -f
    complete -c $command -n '__howto_first_argument' -a 'setup doctor model server shell config help version'
    complete -c $command -n '__howto_help_context' -s h -l help -d 'Show help'
    complete -c $command -n '__howto_query_option_context' -s V -l version -d 'Show version'
    complete -c $command -n '__howto_query_option_context' -s x -l execute -d 'Execute after confirmation'
    complete -c $command -n '__howto_query_option_context' -s e -l execute -d 'Execute after confirmation'
    complete -c $command -n '__howto_query_option_context' -s c -l copy -d 'Copy the generated command'
    complete -c $command -n '__howto_query_option_context' -s q -l quiet -d 'Print only a no-known-risk command'
    complete -c $command -n '__howto_query_option_context' -s n -l count -x -a '1 2 3 4 5 6 7 8' -d 'Generate alternatives'
    complete -c $command -n '__howto_query_option_context' -l json -d 'Emit JSON'
    complete -c $command -n '__howto_query_option_context' -l timing -d 'Show generation timing'
    complete -c $command -n '__howto_management_is doctor' -a '--json --deep'
    complete -c $command -n '__howto_management_is setup' -s y -l yes -d 'Accept setup prompts'
    complete -c $command -n '__howto_management_is setup' -l no-shell -d 'Disable Tab integration'
    complete -c $command -n '__howto_management_is setup' -l shell -x -a 'zsh bash fish' -d 'Configure a shell'
    complete -c $command -n '__howto_management_root_is model' -a 'status install --json --deep'
    complete -c $command -n '__howto_management_action_is model status; or __howto_management_option_led model' -a '--json --deep'
    complete -c $command -n '__howto_management_action_is model install' -a '-y --yes'
    complete -c $command -n '__howto_management_root_is server' -a 'status stop --json'
    complete -c $command -n '__howto_management_action_is server status; or __howto_management_option_led server' -a '--json'
    complete -c $command -n '__howto_management_action_is server stop' -a '--json'
    complete -c $command -n '__howto_management_root_is shell' -a 'init enable disable status'
    complete -c $command -n '__howto_management_action_is shell init' -a 'zsh bash fish'
    complete -c $command -n '__howto_management_action_is shell enable; or __howto_management_action_is shell disable' -l shell -x -a 'zsh bash fish'
    complete -c $command -n '__howto_management_action_is shell status' -a '--json'
    complete -c $command -n '__howto_management_root_is config' -a 'list path get set unset --json'
    complete -c $command -n '__howto_management_action_is config list' -a '--json'
    complete -c $command -n '__howto_config_key_context' -a 'model_path llama_server_path server_url threads context_size max_tokens startup_timeout_seconds show_tab_hint shell_path model_id'
    complete -c $command -n '__howto_show_tab_hint_value_context' -a 'true false'
end
