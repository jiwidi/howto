# HowTo shell integration for Bash 4.1 and newer.
# Empty-line programmable completion leaves the user's Tab binding untouched.

[[ $- == *i* ]] || return 0
[[ -n "${_HOWTO_SHELL_INTEGRATION_LOADED:-}" ]] && return 0
_HOWTO_SHELL_INTEGRATION_LOADED=1

if ((BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 1))); then
  _howto_empty_completion() {
    local pending
    COMPREPLY=()

    if [[ -z "$COMP_LINE" ]] \
      && pending="$(command howto shell take 2>/dev/null)" \
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
  fi
  unset _howto_existing_empty_completion
fi
