# HowTo shell integration for zsh.
# On an empty prompt, Tab inserts the pending command without submitting it.

[[ -o interactive ]] || return 0
[[ -n "${_HOWTO_SHELL_INTEGRATION_LOADED:-}" ]] && return 0
typeset -g _HOWTO_SHELL_INTEGRATION_LOADED=1

typeset _howto_resolved_executable
_howto_resolved_executable="$(whence -p howto 2>/dev/null)" || return 0
[[ "$_howto_resolved_executable" == /* \
  && -x "$_howto_resolved_executable" \
  && "$_howto_resolved_executable" != *$'\n'* \
  && "$_howto_resolved_executable" != *$'\r'* ]] || return 0
typeset -gr _howto_executable="$_howto_resolved_executable"
unset _howto_resolved_executable

export HOWTO_SHELL_SESSION="zsh-$$-$RANDOM-$RANDOM"

# Keep automatic calls attached to the HowTo installation and private state
# that were active when this adapter was sourced. Project-local environment
# tools may later replace these variables; they must not redirect captured
# failed-command text or the one-shot Tab handoff to different state/runtime.
typeset -gra _howto_environment_names=(
  HOME HOWTO_HOME
  XDG_CONFIG_HOME XDG_DATA_HOME XDG_CACHE_HOME XDG_STATE_HOME XDG_RUNTIME_DIR
  TMPDIR TMP TEMP PATH
  HOWTO_MODEL HOWTO_PACKAGED_MODEL HOWTO_LLAMA_SERVER
  LANG LC_ALL LC_CTYPE
  CUDA_VISIBLE_DEVICES HIP_VISIBLE_DEVICES ROCR_VISIBLE_DEVICES
)
typeset -ga _howto_bound_environment=("HOWTO_SHELL_SESSION=$HOWTO_SHELL_SESSION")
typeset _howto_environment_name
for _howto_environment_name in "${_howto_environment_names[@]}"; do
  if [[ ${parameters[$_howto_environment_name]-} == *export* ]]; then
    _howto_bound_environment+=(
      "$_howto_environment_name=${(P)_howto_environment_name}"
    )
  fi
done
typeset -gr _howto_env_executable=/usr/bin/env
typeset -gra _howto_environment=("${_howto_bound_environment[@]}")
unset _howto_bound_environment _howto_environment_name

typeset -gA _howto_tab_fallbacks
typeset -g _howto_advisor_command=
typeset -gi _howto_advisor_armed=0
typeset -gi _howto_advisor_running=0

_howto_advisor_preexec() {
  emulate -L zsh
  (( _howto_advisor_running )) && return 0
  _howto_advisor_command="$1"
  _howto_advisor_armed=1
  return 0
}

_howto_advisor_precmd() {
  local failed_status=$?
  emulate -L zsh
  local command_line="$_howto_advisor_command"

  (( _howto_advisor_armed )) || return 0
  _howto_advisor_armed=0
  _howto_advisor_command=

  (( _howto_advisor_running )) && return 0
  (( failed_status != 0 && failed_status < 128 )) || return 0
  [[ -n "$command_line" && "$command_line" != [[:space:]]* ]] || return 0
  [[ "$command_line" != *$'\n'* && "$command_line" != *$'\r'* ]] || return 0
  [[ ! "$command_line" =~ '^(command[[:space:]]+)?([^[:space:]]*/)?howto([[:space:]]|$)' ]] || return 0
  [[ ! "$command_line" =~ '^(exit|logout|return)([[:space:]]|$)' ]] || return 0

  # This check accepts no command input. A disabled or unavailable advisor
  # therefore cannot receive the captured line, including after a live toggle.
  command "$_howto_env_executable" -i "${_howto_environment[@]}" \
    "$_howto_executable" shell advisor-ready </dev/null >/dev/null 2>&1 || return 0

  _howto_advisor_running=1
  {
    printf %s "$command_line" |
      command "$_howto_env_executable" -i "${_howto_environment[@]}" \
        "$_howto_executable" shell advise --status "$failed_status"
  } always {
    _howto_advisor_running=0
  }
  return 0
}

autoload -Uz add-zsh-hook
add-zsh-hook preexec _howto_advisor_preexec
add-zsh-hook precmd _howto_advisor_precmd

for _howto_keymap in main emacs viins; do
  _howto_binding="$(bindkey -M "$_howto_keymap" $'\t' 2>/dev/null)" || continue
  _howto_widget="${_howto_binding##* }"
  if [[ -n "$_howto_widget" && "$_howto_widget" != howto-tab ]]; then
    _howto_tab_fallbacks[$_howto_keymap]="$_howto_widget"
  fi
done
unset _howto_binding _howto_keymap _howto_widget

_howto_tab_widget() {
  if [[ -z "$BUFFER" ]]; then
    local pending
    if pending="$(command "$_howto_env_executable" -i \
      "${_howto_environment[@]}" "$_howto_executable" shell take 2>/dev/null)" \
      && [[ -n "$pending" \
        && "$pending" != *$'\n'* \
        && "$pending" != *$'\r'* \
        && "$pending" != *$'\t'* \
        && "$pending" != *$'\e'* ]]; then
      BUFFER="$pending"
      CURSOR=${#BUFFER}
      return 0
    fi
  fi

  local fallback="${_howto_tab_fallbacks[$KEYMAP]:-expand-or-complete}"
  if [[ "$fallback" == howto-tab || -z "${widgets[$fallback]}" ]]; then
    fallback=expand-or-complete
  fi
  zle "$fallback"
}

zle -N howto-tab _howto_tab_widget
bindkey -M emacs $'\t' howto-tab
bindkey -M viins $'\t' howto-tab
