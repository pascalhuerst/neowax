#!/usr/bin/env bash
# Create a release: bump the version, commit it and tag the commit.
#
# Usage: scripts/release.sh [--dry-run] [--push] <version>
#
#   <version>   new version, e.g. 0.1.7 or v0.1.7
#   --dry-run   only check and print what would be done
#   --push      also push the commit and the tag (the tag starts the release build)
#
# Without --push nothing leaves your machine; the commands to publish are printed.

set -euo pipefail

usage() {
    sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
    exit "${1:-1}"
}

dry_run=0
push=0
version=""
for arg in "$@"; do
    case "$arg" in
        --dry-run) dry_run=1 ;;
        --push) push=1 ;;
        -h|--help) usage 0 ;;
        -*) echo "unknown option: $arg" >&2; usage ;;
        *) [ -z "$version" ] || { echo "only one version allowed" >&2; usage; }
           version="${arg#v}" ;;
    esac
done
[ -n "$version" ] || usage

die() { echo "error: $*" >&2; exit 1; }

if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]]; then
    die "'$version' is not a valid version (expected e.g. 1.2.3)"
fi
tag="v$version"

cd "$(git rev-parse --show-toplevel)"

# ---- checks -----------------------------------------------------------------

branch="$(git rev-parse --abbrev-ref HEAD)"
[ "$branch" = "main" ] || die "releases are made from main (you are on '$branch')"

if ! git diff --quiet || ! git diff --cached --quiet; then
    die "there are uncommitted changes to tracked files; commit or stash them first"
fi

current="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1)"
[ -n "$current" ] || die "cannot read the current version from Cargo.toml"

# The new version must be strictly greater than the current one
if [ "$current" = "$version" ] || [ "$(printf '%s\n%s\n' "$current" "$version" | sort -V | tail -n1)" != "$version" ]; then
    die "new version $version must be greater than the current $current"
fi

if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    die "tag $tag already exists"
fi

if git remote get-url origin >/dev/null 2>&1; then
    git fetch --quiet origin main --tags || die "cannot fetch from origin"
    if git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1; then
        die "tag $tag already exists on origin"
    fi
    behind="$(git rev-list --count HEAD..origin/main)"
    [ "$behind" = 0 ] || die "main is $behind commit(s) behind origin/main; pull first"
fi

echo "Releasing $current -> $version (tag $tag)"
if [ "$dry_run" = 1 ]; then
    echo "dry run: would bump Cargo.toml and Cargo.lock, commit 'Release $tag' and tag $tag"
    [ "$push" = 0 ] || echo "dry run: would push main and $tag to origin"
    exit 0
fi

# ---- bump, commit, tag --------------------------------------------------------

# Only the first `version = "..."` line is the package version
awk -v v="$version" '!done && /^version = "/ { print "version = \"" v "\""; done=1; next } { print }' \
    Cargo.toml > Cargo.toml.new
mv Cargo.toml.new Cargo.toml

# Refresh the lock file for the new version without touching dependencies
cargo update --workspace --quiet

if [ "$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1)" != "$version" ]; then
    git checkout -- Cargo.toml Cargo.lock
    die "Cargo.toml was not updated as expected; changes reverted"
fi

git add Cargo.toml Cargo.lock
git commit --quiet -m "Release $tag"
git tag -a "$tag" -m "neowax $tag"

echo "Created commit 'Release $tag' and tag $tag."

if [ "$push" = 1 ]; then
    git push origin main
    git push origin "$tag"
    echo "Pushed. The release build is now running on GitHub."
else
    echo
    echo "To publish (this starts the release build):"
    echo "  git push origin main && git push origin $tag"
    echo "To undo instead:"
    echo "  git tag -d $tag && git reset --hard HEAD~1"
fi
