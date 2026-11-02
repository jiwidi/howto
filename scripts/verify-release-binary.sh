#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <binary> [bundle-root]" >&2
  exit 64
}

[[ $# -eq 1 || $# -eq 2 ]] || usage

binary="$1"
bundle_root="${2:-}"

if [[ ! -f "$binary" || ! -x "$binary" ]]; then
  echo "error: release binary is missing or not executable: $binary" >&2
  exit 65
fi

canonical_path() {
  local path="$1"
  local directory
  directory="$(cd -P -- "$(dirname -- "$path")" && pwd)"
  printf '%s/%s\n' "$directory" "$(basename -- "$path")"
}

binary="$(canonical_path "$binary")"

if [[ -n "$bundle_root" ]]; then
  if [[ ! -d "$bundle_root" ]]; then
    echo "error: bundle root is not a directory: $bundle_root" >&2
    exit 65
  fi

  executable_count=0
  matching_binary_count=0
  while IFS= read -r -d '' candidate; do
    executable_count=$((executable_count + 1))
    if [[ "$(canonical_path "$candidate")" == "$binary" ]]; then
      matching_binary_count=$((matching_binary_count + 1))
    else
      echo "error: unexpected executable in release bundle: $candidate" >&2
    fi
  done < <(find "$bundle_root" -type f -perm -0111 -print0)

  if [[ "$executable_count" -ne 1 || "$matching_binary_count" -ne 1 ]]; then
    echo "error: release bundle must contain exactly one executable (howto)" >&2
    exit 66
  fi
fi

reject_embedded_runtime_library() {
  local dependency="$1"
  local filename="${dependency##*/}"
  case "$filename" in
    libllama.* | libllama-* | libggml.* | libggml-*)
      echo "error: release binary dynamically links bundled runtime library: $dependency" >&2
      return 1
      ;;
  esac
}

case "$(uname -s)" in
  Darwin)
    command -v otool >/dev/null
    while IFS= read -r dependency; do
      [[ -n "$dependency" ]] || continue
      reject_embedded_runtime_library "$dependency"
      case "$dependency" in
        /System/Library/* | /usr/lib/*) ;;
        *)
          echo "error: release binary has a non-system runtime dependency: $dependency" >&2
          exit 67
          ;;
      esac
    done < <(otool -L "$binary" | sed '1d' | awk '{ print $1 }')
    ;;
  Linux)
    command -v readelf >/dev/null
    command -v ldd >/dev/null

    while IFS= read -r dependency; do
      [[ -n "$dependency" ]] || continue
      reject_embedded_runtime_library "$dependency"
    done < <(readelf -d "$binary" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p')

    if ! linkage="$(ldd "$binary" 2>&1)"; then
      if grep -Fq "not a dynamic executable" <<<"$linkage"; then
        linkage=""
      else
        printf '%s\n' "$linkage" >&2
        exit 67
      fi
    fi
    if grep -Fq "not found" <<<"$linkage"; then
      printf '%s\n' "$linkage" >&2
      echo "error: release binary has an unresolved runtime dependency" >&2
      exit 67
    fi

    while IFS= read -r dependency; do
      [[ -n "$dependency" ]] || continue
      case "$dependency" in
        /lib/* | /lib64/* | /usr/lib/* | /usr/lib64/*) ;;
        *)
          echo "error: release binary has a non-system runtime dependency: $dependency" >&2
          exit 67
          ;;
      esac
    done < <(
      awk '
        $2 == "=>" && $3 ~ /^\// { print $3 }
        $1 ~ /^\// { print $1 }
      ' <<<"$linkage"
    )
    ;;
  *)
    echo "error: unsupported release platform: $(uname -s)" >&2
    exit 68
    ;;
esac

echo "verified self-contained release binary: $binary"
