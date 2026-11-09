#!/bin/bash
# Construit dist/Euhpen-Setup-<version>-macos-arm64.dmg : une image disque qui contient
# « Installer Euhpen.app », lequel embarque toute l'application.

set -euo pipefail
cd "$(dirname "$0")/../.."
root=$(pwd)

if [ "$(uname -s)" != "Darwin" ] || [ "$(uname -m)" != "arm64" ]; then
  echo "Ce script se lance sur un Mac à puce Apple." >&2
  exit 1
fi

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
target=${CARGO_TARGET_DIR:-$root/target}
work=$target/installer-macos
stage=$work/application
image=$work/image
archive=$work/application.zip
here=packaging/macos

# Pose un paquet .app minimal puis le signe sans identité : une puce Apple refuse tout binaire non signé.
make_app() { # dossier du paquet, programme, nom du programme, Info.plist
  mkdir -p "$1/Contents/MacOS"
  cp "$2" "$1/Contents/MacOS/$3"
  chmod 755 "$1/Contents/MacOS/$3"
  sed "s/__VERSION__/$version/g" "$4" > "$1/Contents/Info.plist"
  /usr/bin/codesign --force --sign - "$1"
}

echo "1/4  Compilation d'Euhpen"
cargo build --release --locked --bins

echo "2/4  Préparation de l'application"
rm -rf "$work"
mkdir -p "$stage/python" "$image"
make_app "$stage/Euhpen.app" "$target/release/euhpen-gui" euhpen-gui "$here/Euhpen.Info.plist"
cp "$target/release/euhpen" "$stage/euhpen"
cp LICENSE "$stage/"
cp python/requirements.txt "$stage/python/"
cp -R python/euhpen "$stage/python/"
find "$stage/python" -type d -name __pycache__ -prune -exec rm -rf {} +
(cd "$stage" && /usr/bin/zip -qrX "$archive" .)

echo "3/4  Compilation de l'installateur"
EUHPEN_PAYLOAD=$archive cargo build --release --locked --features installer --bin euhpen-setup

echo "4/4  Création de l'image disque"
make_app "$image/Installer Euhpen.app" "$target/release/euhpen-setup" euhpen-setup "$here/Installer.Info.plist"
cp "$here/LISEZ-MOI.txt" "$image/LISEZ-MOI.txt"
mkdir -p dist
output=$root/dist/Euhpen-Setup-$version-macos-arm64.dmg
rm -f "$output"
/usr/bin/hdiutil create -quiet -volname "Euhpen $version" -srcfolder "$image" -fs HFS+ -format UDZO "$output"

echo "Installateur prêt : $output ($(du -h "$output" | cut -f1))"
