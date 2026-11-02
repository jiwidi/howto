# HowTo shell integration for Bash 4.1 and newer.
# Empty-line programmable completion leaves the user's Tab binding untouched.

[[ $- == *i* ]] || return 0
[[ -n "${_HOWTO_SHELL_INTEGRATION_LOADED:-}" ]] && return 0
_HOWTO_SHELL_INTEGRATION_LOADED=1

_howto_resolved_executable="$(type -P howto 2>/dev/null)" || return 0
[[ "$_howto_resolved_executable" == /* \
  && -x "$_howto_resolved_executable" \
  && "$_howto_resolved_executable" != *$'\n'* \
  && "$_howto_resolved_executable" != *$'\r'* ]] || return 0
readonly _howto_executable="$_howto_resolved_executable"
unset _howto_resolved_executable

readonly _howto_env_executable=/usr/bin/env
readonly -a _howto_environment_names=(
  HOME HOWTO_HOME
  XDG_CONFIG_HOME XDG_DATA_HOME XDG_CACHE_HOME XDG_STATE_HOME XDG_RUNTIME_DIR
  TMPDIR TMP TEMP PATH
  HOWTO_MODEL HOWTO_PACKAGED_MODEL HOWTO_LLAMA_SERVER
  LANG LC_ALL LC_CTYPE
  CUDA_VISIBLE_DEVICES HIP_VISIBLE_DEVICES ROCR_VISIBLE_DEVICES
)

_howto_bind_environment() {
  local name declaration
  _howto_bound_environment=("HOWTO_SHELL_SESSION=$HOWTO_SHELL_SESSION")
  for name in "${_howto_environment_names[@]}"; do
    declaration=$(declare -p "$name" 2>/dev/null) || continue
    [[ "$declaration" == 'declare -'*x* ]] || continue
    _howto_bound_environment+=("$name=${!name}")
  done
  readonly -a _howto_environment=("${_howto_bound_environment[@]}")
  unset _howto_bound_environment
}

if ((BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 1))); then
  _howto_empty_completion() {
    local pending
    COMPREPLY=()

    if [[ -z "$COMP_LINE" ]] \
      && pending="$(command "$_howto_env_executable" -i \
        "${_howto_environment[@]}" "$_howto_executable" shell take 2>/dev/null)" \
      && [[ -n "$pending" \
        && "$pending" != *$'\n'* \
        && "$pending" != *$'\r'* \
        && "$pending" != *$'\t'* \
        && "$pending" != *$'\e'* ]]; then
      COMPREPLY=("$pending")
      compopt -o noquote -o nospace +o default +o bashdefault
      return 0
    fi

    compopt +o noquote +o nospace -o default -o bashdefault
  }

  _howto_existing_empty_completion="$(complete -p -E 2>/dev/null)" ||
    _howto_existing_empty_completion=
  if [[ -z "$_howto_existing_empty_completion" \
    || "$_howto_existing_empty_completion" == *'_howto_empty_completion'* ]]; then
    complete -E -o default -o bashdefault -F _howto_empty_completion
    export HOWTO_SHELL_SESSION="bash-$$-$RANDOM-$RANDOM"
    _howto_bind_environment
  fi
  unset _howto_existing_empty_completion

  # PROMPT_COMMAND arrays preserve the original status for every hook. They
  # were added in Bash 5.1; older supported Bash versions keep Tab insertion
  # but do not install the failed-command advisor.
  if ((BASH_VERSINFO[0] > 5 \
    || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1))) \
    && [[ ${HOWTO_SHELL_SESSION:-} == bash-* ]]; then
    _howto_advisor_running=0
    _howto_advisor_last_histcmd=$((HISTCMD - 1))

    _howto_failed_command_advisor() {
      local failed_status=$?
      local history_id command_line
      local HISTTIMEFORMAT=

      history_id=$((HISTCMD - 1))
      if [[ ! -o history || "$history_id" == "$_howto_advisor_last_histcmd" ]]; then
        _howto_advisor_last_histcmd=$history_id
        return 0
      fi
      _howto_advisor_last_histcmd=$history_id

      (( _howto_advisor_running == 0 )) || return 0
      (( failed_status != 0 && failed_status < 128 )) || return 0

      command "$_howto_env_executable" -i "${_howto_environment[@]}" \
        "$_howto_executable" shell advisor-ready </dev/null >/dev/null 2>&1 || return 0

      command_line="$(builtin history 1)" || return 0
      if [[ "$command_line" =~ ^[[:space:]]*[0-9]+[*[:space:]][[:space:]](.*)$ ]]; then
        command_line=${BASH_REMATCH[1]}
      else
        return 0
      fi

      [[ "$command_line" != *$'\n'* ]] || return 0
      [[ -n "$command_line" && "$command_line" != [[:space:]]* ]] || return 0
      [[ "$command_line" != *$'\r'* ]] || return 0
      [[ ! "$command_line" =~ ^(command[[:space:]]+)?([^[:space:]]*/)?howto([[:space:]]|$) ]] || return 0
      [[ ! "$command_line" =~ ^(exit|logout|return)([[:space:]]|$) ]] || return 0

      _howto_advisor_running=1
      printf %s "$command_line" |
        command "$_howto_env_executable" -i "${_howto_environment[@]}" \
          "$_howto_executable" shell advise --status "$failed_status"
      _howto_advisor_running=0
      return 0
    }

    _howto_existing_prompt_commands=()
    if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a '* ]]; then
      _howto_existing_prompt_commands=("${PROMPT_COMMAND[@]}")
    elif [[ -n ${PROMPT_COMMAND:-} ]]; then
      _howto_existing_prompt_commands=("$PROMPT_COMMAND")
    fi
    PROMPT_COMMAND=(_howto_failed_command_advisor "${_howto_existing_prompt_commands[@]}")
    unset _howto_existing_prompt_commands
  fi
fi
