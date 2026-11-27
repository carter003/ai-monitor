#!/usr/bin/env bash
# Install a tested Linux build alongside the current user's standalone release.
set -euo pipefail
binary=$(realpath "${1:?usage: install.sh /path/to/patched/codex}")
here=$(dirname "$(realpath "$0")")
entry="$HOME/.local/bin/codex"
root="$HOME/.codex/packages/standalone"
official="$root/current/bin/codex"
base=$(realpath "$root/current")
patches="$root/releases/.local-patches"
case "$(readlink "$entry")" in
    "$official"|"$patches"/*/launch) ;;
    *) echo "Unexpected Codex entry: $entry" >&2; exit 1 ;;
esac
version=$("$base/bin/codex" --version)
[[ $("$binary" --version) == "$version" ]]
digest=$(sha256sum "$binary" | cut -d' ' -f1)
dest="$patches/$(basename "$base")-$digest"
mkdir -p "$patches"
work=$(mktemp -d "$patches/.install.XXXXXXXX")
trap 'rm -rf -- "$work"' EXIT
if [[ ! -d "$dest" ]]; then
    mkdir -p "$work/package/bin"
    cp -a --reflink=auto "$base"/{codex-package.json,codex-path,codex-resources} "$work/package/"
    for helper in "$base"/bin/*; do
        [[ $(basename "$helper") == codex ]] || cp -a --reflink=auto "$helper" "$work/package/bin/"
    done
    install -m755 "$binary" "$work/package/bin/codex"
    install -m755 "$here/launch" "$work/package/launch"
    ln -s "$base/bin/codex" "$work/package/upstream"
    ln -s "$official" "$work/package/official-current"
    mv -T "$work/package" "$dest"
fi
[[ "$official" -ef "$dest/upstream" ]]
[[ $(sha256sum "$dest/bin/codex" | cut -d' ' -f1) == "$digest" ]]
# Use the entrypoint's filesystem for the atomic rename.
linkdir=$(mktemp -d "$(dirname "$entry")/.codex-cli-install.XXXXXXXX")
trap 'rm -rf -- "$work" "$linkdir"' EXIT
ln -s "$dest/launch" "$linkdir/codex"
mv -Tf "$linkdir/codex" "$entry"
printf '%s\n' "$dest"
