_howto_complete_words() {
  local words=$1 current=$2 candidate
  while IFS= read -r candidate; do
    COMPREPLY[${#COMPREPLY[@]}]=$candidate
  done < <(compgen -W "$words" -- "$current")
}

_howto() {
  local current previous first second
  COMPREPLY=()
  current="${COMP_WORDS[COMP_CWORD]}"
  previous="${COMP_WORDS[COMP_CWORD-1]}"
  first="${COMP_WORDS[1]}"
  second="${COMP_WORDS[2]}"

  if [[ $previous == -n || $previous == --count ]]; then
    _howto_complete_words "1 2 3 4 5 6 7 8" "$current"
  elif [[ $COMP_CWORD -eq 1 ]]; then
    _howto_complete_words \
      "setup doctor model server shell config help version -h --help -V --version -x -e --execute -c --copy -q --quiet -n --count --json --timing --" \
      "$current"
  elif [[ $first == setup && $COMP_CWORD -ge 2 ]]; then
    if [[ $previous == --shell ]]; then
      _howto_complete_words "zsh bash fish" "$current"
    else
      _howto_complete_words "-y --yes --no-shell --shell --help" "$current"
    fi
  elif [[ $first == doctor && $COMP_CWORD -eq 2 ]]; then
    _howto_complete_words "--json --deep" "$current"
  elif [[ $first == model && $COMP_CWORD -eq 2 ]]; then
    _howto_complete_words "status install --json --deep --help" "$current"
  elif [[ $first == model && $second == status ]]; then
    _howto_complete_words "--json --deep" "$current"
  elif [[ $first == model && $second == install ]]; then
    _howto_complete_words "-y --yes" "$current"
  elif [[ $first == server && $COMP_CWORD -eq 2 ]]; then
    _howto_complete_words "status stop --json --help" "$current"
  elif [[ $first == server && ( $second == status || $second == stop ) ]]; then
    _howto_complete_words "--json" "$current"
  elif [[ $first == shell && $COMP_CWORD -eq 2 ]]; then
    _howto_complete_words "init enable disable status --help" "$current"
  elif [[ $first == shell && $second == init && $COMP_CWORD -eq 3 ]]; then
    _howto_complete_words "zsh bash fish" "$current"
  elif [[ $first == shell && ( $second == enable || $second == disable ) ]]; then
    if [[ $previous == --shell ]]; then
      _howto_complete_words "zsh bash fish" "$current"
    else
      _howto_complete_words "--shell" "$current"
    fi
  elif [[ $first == shell && $second == status ]]; then
    _howto_complete_words "--json" "$current"
  elif [[ $first == config && $COMP_CWORD -eq 2 ]]; then
    _howto_complete_words "list path get set unset --json --help" "$current"
  elif [[ $first == config && $second == list ]]; then
    _howto_complete_words "--json" "$current"
  elif [[ $first == config && ( $second == get || $second == set || $second == unset ) && $COMP_CWORD -eq 3 ]]; then
    _howto_complete_words \
      "model_path llama_server_path server_url threads context_size max_tokens startup_timeout_seconds show_tab_hint shell_path model_id" \
      "$current"
  elif [[ $first == config && $second == set && ${COMP_WORDS[3]} == show_tab_hint && $COMP_CWORD -eq 4 ]]; then
    _howto_complete_words "true false" "$current"
  fi
}

complete -F _howto howto
