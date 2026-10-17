#!/usr/bin/env bash
# Version helpers for .github/workflows/release.yml.
#
#   release-version.sh next <patch|minor|major> [explicit-version]
#       Print the version (no leading "v") the next release should have.
#       An explicit version wins over the bump. With no v*.*.* tag yet, the
#       first release uses the version already in Cargo.toml as it is.
#   release-version.sh apply <version>
#       Write <version> into [workspace.package] in Cargo.toml.
#
# Needs a git checkout with the tags fetched. Errors go to stderr, exit 1.
set -euo pipefail

SEMVER='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'

fail() {
  echo "release-version: $*" >&2
  exit 1
}

# The workspace version in Cargo.toml (first top-level `version = "…"`).
cargo_version() {
  sed -n 's/^version = "\([^"]*\)".*/\1/p' Cargo.toml | head -n 1
}

# Newest vX.Y.Z[-pre] tag without the "v", or nothing.
latest_tag_version() {
  git tag --list 'v[0-9]*.[0-9]*.[0-9]*' --sort=-v:refname | head -n 1 | sed 's/^v//'
}

# "1.2.3-beta.1" -> "1.2.3"
core() {
  echo "${1%%-*}"
}

next() {
  local bump="${1:-}" explicit="${2:-}" latest version
  latest="$(latest_tag_version)"

  if [[ -n "$explicit" ]]; then
    version="${explicit#v}"
    [[ "$version" =~ $SEMVER ]] || fail "'$explicit' is not a version like 1.2.3 or 1.2.3-beta.1"
  elif [[ -z "$latest" ]]; then
    version="$(cargo_version)"
    [[ "$version" =~ $SEMVER ]] || fail "cannot read a version from Cargo.toml"
  else
    local major minor patch
    IFS=. read -r major minor patch <<<"$(core "$latest")"
    case "$bump" in
      major) version="$((major + 1)).0.0" ;;
      minor) version="${major}.$((minor + 1)).0" ;;
      patch) version="${major}.${minor}.$((patch + 1))" ;;
      *) fail "bump must be patch, minor or major (got '$bump')" ;;
    esac
  fi

  if git rev-parse -q --verify "refs/tags/v${version}" >/dev/null; then
    fail "tag v${version} already exists"
  fi
  # Guard against a typo that would release "older" than what exists.
  if [[ -n "$latest" ]]; then
    local lowest
    lowest="$(printf '%s\n%s\n' "$(core "$latest")" "$(core "$version")" | sort -V | head -n 1)"
    if [[ "$lowest" != "$(core "$latest")" ]]; then
      fail "${version} is older than the latest release ${latest}"
    fi
  fi
  echo "$version"
}

apply() {
  local version="${1:-}" tmp
  [[ "$version" =~ $SEMVER ]] || fail "'$version' is not a valid version"
  # Only the first `version = "…"` line: the one under [workspace.package].
  # awk + mv instead of `sed -i`, which GNU knows a way to stop after the
  # first match ("0,/re/") but BSD sed does not.
  tmp="$(mktemp)" || fail "could not create a temp file"
  awk -v v="$version" '
    !done && /^version = "/ {
      sub(/^version = ".*"/, "version = \"" v "\"")
      done = 1
    }
    { print }
  ' Cargo.toml > "$tmp"
  mv "$tmp" Cargo.toml
  [[ "$(cargo_version)" == "$version" ]] || fail "could not set the version in Cargo.toml"
}

case "${1:-}" in
  next) shift; next "$@" ;;
  apply) shift; apply "$@" ;;
  *) fail "usage: release-version.sh next <patch|minor|major> [version] | apply <version>" ;;
esac
