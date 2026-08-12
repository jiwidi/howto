# HowTo shell integration for zsh.
# On an empty prompt, Tab inserts the pending command without submitting it.

[[ -o interactive ]] || return 0
[[ -n "${_HOWTO_SHELL_INTEGRATION_LOADED:-}" ]] && return 0
typeset -g _HOWTO_SHELL_INTEGRATION_LOADED=1

export HOWTO_SHELL_SESSION="zsh-$$-$RANDOM-$RANDOM"

typeset -gA _howto_tab_fallbacks

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
    if pending="$(command howto shell take 2>/dev/null)" \
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
