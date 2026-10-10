#!/usr/bin/env bash
# #### PR #42: the HiveOS, mmpOS and RaveOS packages, made from the Linux
# x86_64 release archive (its pickaxe, cuda/, hip/, BACKENDS.txt, README.md
# and licenses) and packaging/farm-os/.
# Usage: tools/farm-os/build.sh <linux-x86_64.tar.gz> <out-dir>
set -euo pipefail
archive=$1
mkdir -p "$2"
out=$(cd "$2" && pwd)
root=$(cd "$(dirname "$0")/../.." && pwd)
packaging="$root/packaging/farm-os"

if grep -rlq --exclude-dir=__pycache__ $'\r' "$packaging"; then
  echo "farm-os scripts must use LF line endings" >&2
  exit 1
fi
version=$(awk -F '"' '/^version = / { print $2; exit }' "$root/Cargo.toml")
version=${version//-/_}

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
# Through stdin and stdout, so a path with a colon is never a remote host.
tar -xzf - -C "$stage" < "$archive"
src=$(find "$stage" -mindepth 1 -maxdepth 1 -type d | head -n 1)
test -x "$src/pickaxe"

# The miner's files, into $1.
payload() {
  install -m 0755 "$src/pickaxe" "$1/pickaxe"
  for item in cuda hip BACKENDS.txt README.md; do
    if [[ -e "$src/$item" ]]; then
      cp -R "$src/$item" "$1/"
    fi
  done
  for license in "$src"/LICENSE*; do
    if [[ -e "$license" ]]; then
      cp "$license" "$1/"
    fi
  done
}

# Copies the OS's files from $1 into $2, with the version, scripts 0755.
scripts() {
  for file in "$1"/*; do
    [[ -f "$file" ]] || continue
    name=$(basename "$file")
    sed "s/@VERSION@/$version/g" "$file" > "$2/$name"
    case "$name" in
      *.sh | *.py) chmod 0755 "$2/$name" ;;
      *) chmod 0644 "$2/$name" ;;
    esac
  done
}

mkdir -p "$stage/out"
for os in hiveos mmpos; do
  folder="$stage/out/pickaxe-$os"
  mkdir -p "$folder"
  payload "$folder"
  scripts "$packaging/$os" "$folder"
  tar --owner=0 --group=0 -C "$stage/out" -czf - "pickaxe-$os" > "$out/pickaxe-$os-$version.tar.gz"
done

rave="$stage/out/raveos"
mkdir -p "$rave/RAVINOS"
payload "$rave"
scripts "$packaging/raveos/RAVINOS" "$rave/RAVINOS"
# A zip with unix modes, from Python's standard library (no zip program).
"${PYTHON:-python3}" - "$rave" "$out/pickaxe-raveos-$version.zip" <<'PY'
import os
import sys
import zipfile

root, target = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(target, "w", zipfile.ZIP_DEFLATED) as package:
    for folder, _, files in sorted(os.walk(root)):
        for name in sorted(files):
            path = os.path.join(folder, name)
            info = zipfile.ZipInfo(os.path.relpath(path, root).replace(os.sep, "/"))
            executable = name == "pickaxe" or name.endswith((".py", ".sh"))
            info.external_attr = (0o100755 if executable else 0o100644) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            with open(path, "rb") as handle:
                package.writestr(info, handle.read())
PY

ls -l "$out"
