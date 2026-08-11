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
      "doctor model server config help version -h --help -V --version -x -e --execute -c --copy -q --quiet -n --count --json --timing --" \
      "$current"
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
  elif [[ $first == config && $COMP_CWORD -eq 2 ]]; then
    _howto_complete_words "list path get set unset --json --help" "$current"
  elif [[ $first == config && $second == list ]]; then
    _howto_complete_words "--json" "$current"
  fi
}

complete -F _howto howto
