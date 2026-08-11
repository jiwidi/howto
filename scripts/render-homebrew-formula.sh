#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 <version-or-tag> <source-sha256> <output-path>" >&2
  exit 64
}

[[ $# -eq 3 ]] || usage

version="${1#v}"
source_sha256="$2"
output_path="$3"

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo "error: invalid release version: $version" >&2
  exit 65
fi

if [[ ! "$source_sha256" =~ ^[0-9a-f]{64}$ ]]; then
  echo "error: source SHA-256 must be 64 lowercase hexadecimal characters" >&2
  exit 65
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "$script_dir/.." && pwd)"
template="$repository_root/Formula/howto.rb"
source_url="https://github.com/jiwidi/howto/releases/download/v${version}/howto-${version}-source.tar.gz"

if grep -Eq '^  (url|sha256) ' "$template"; then
  echo "error: formula template already contains a stable source" >&2
  exit 66
fi

mkdir -p "$(dirname "$output_path")"
temporary_path="$(mktemp "${output_path}.tmp.XXXXXX")"
trap 'rm -f "$temporary_path"' EXIT

awk -v source_url="$source_url" -v source_sha256="$source_sha256" '
  /^  license / && !inserted_source {
    print "  url \"" source_url "\""
    print "  sha256 \"" source_sha256 "\""
    print ""
    inserted_source = 1
  }
  { print }
  /^  head / && !inserted_livecheck {
    print ""
    print "  livecheck do"
    print "    url :stable"
    print "    strategy :github_latest"
    print "  end"
    inserted_livecheck = 1
  }
  END {
    if (!inserted_source || !inserted_livecheck) {
      exit 1
    }
  }
' "$template" > "$temporary_path"

chmod 0644 "$temporary_path"
mv "$temporary_path" "$output_path"
trap - EXIT
