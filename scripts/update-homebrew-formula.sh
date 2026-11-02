#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <version-or-tag> <source-sha256> <formula-path>" >&2
  exit 64
}

[[ $# -eq 3 ]] || usage

version="${1#v}"
source_sha256="$2"
formula_path="$3"

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo "error: invalid release version: $version" >&2
  exit 65
fi
if [[ ! "$source_sha256" =~ ^[0-9a-f]{64}$ ]]; then
  echo "error: source SHA-256 must be 64 lowercase hexadecimal characters" >&2
  exit 65
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source_url="https://github.com/jiwidi/howto/releases/download/v${version}/howto-${version}-source.tar.gz"

if [[ -f "$formula_path" ]] && grep -Fqx "  url \"$source_url\"" "$formula_path"; then
  if ! grep -Fqx "  sha256 \"$source_sha256\"" "$formula_path"; then
    echo "error: formula URL matches but its immutable checksum differs" >&2
    exit 66
  fi
  echo "formula already points at this immutable source; preserving bottle metadata"
  exit 0
fi

"$script_dir/render-homebrew-formula.sh" \
  "$version" \
  "$source_sha256" \
  "$formula_path"
