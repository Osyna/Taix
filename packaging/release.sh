#!/bin/sh
# Package prebuilt binaries into tarballs, .deb, .rpm and .pkg.tar.zst.
# Usage: packaging/release.sh <bin-dir> <out-dir>
#   <bin-dir> holds `taix` (static musl CLI) and `taix-gui` (Ubuntu 24.04 build).
# Needs: nfpm (tested with 2.48.0), tar, gzip, sha256sum.
set -eu

[ $# -eq 2 ] || { echo "usage: $0 <bin-dir> <out-dir>" >&2; exit 2; }
root=$(cd "$(dirname "$0")/.." && pwd)
bin=$(cd "$1" && pwd)
mkdir -p "$2"
out=$(cd "$2" && pwd)
command -v nfpm >/dev/null || { echo "nfpm not found" >&2; exit 1; }
for f in taix taix-gui; do [ -x "$bin/$f" ] || { echo "missing executable $bin/$f" >&2; exit 1; }; done

VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' "$root/Cargo.toml")
[ -n "$VERSION" ] || { echo "no version in Cargo.toml" >&2; exit 1; }
export VERSION
actual=$("$bin/taix" --version)
[ "$actual" = "taix $VERSION" ] || { echo "binary version '$actual' does not match $VERSION" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
STAGE=$work/stage
export STAGE
mkdir "$STAGE"
cp "$bin/taix" "$bin/taix-gui" "$root/LICENSE" "$root/README.md" "$STAGE/"
cp "$root/crates/taix-web/assets/icon.svg" "$STAGE/dev.taix.TaiX.svg"
cp "$root/crates/taix-web/assets/icon-512.png" "$STAGE/dev.taix.TaiX.png"
sed 's/^Icon=.*/Icon=dev.taix.TaiX/' "$root/packaging/dev.taix.TaiX.desktop" > "$STAGE/dev.taix.TaiX.desktop"

# Portable tarballs: <name>/ with binary, docs, INSTALL.txt (and desktop files for the GUI).
tarball() { # name, files...
	name=$1; shift
	d=$work/$name
	mkdir "$d"
	for f in "$@"; do cp "$STAGE/$f" "$d/"; done
	cp "$work/INSTALL-$name.txt" "$d/INSTALL.txt"
	tar --sort=name --owner=0 --group=0 --numeric-owner -C "$work" -czf "$out/$name.tar.gz" "$name"
}

cat > "$work/INSTALL-taix-x86_64-linux-musl.txt" <<EOF
TaiX $VERSION CLI/TUI, static x86_64 Linux binary (any distro). Requires tmux.

  install -Dm755 taix ~/.local/bin/taix      # or: sudo install -Dm755 taix /usr/local/bin/taix
  taix --help
EOF
cat > "$work/INSTALL-taix-gui-x86_64-linux-gnu.txt" <<EOF
TaiX $VERSION desktop app, x86_64 glibc. Built on Ubuntu 24.04: needs glibc >= 2.39,
GTK 4.12+, libadwaita 1.5+, WebKitGTK 6.0 (Ubuntu 24.04+, Debian 13+, current Fedora/Arch).
Also install the CLI (taix-x86_64-linux-musl.tar.gz): the app needs tmux and the taix binary.

  install -Dm755 taix-gui ~/.local/bin/taix-gui
  install -Dm644 dev.taix.TaiX.desktop ~/.local/share/applications/dev.taix.TaiX.desktop
  install -Dm644 dev.taix.TaiX.svg ~/.local/share/icons/hicolor/scalable/apps/dev.taix.TaiX.svg
  install -Dm644 dev.taix.TaiX.png ~/.local/share/icons/hicolor/512x512/apps/dev.taix.TaiX.png

(use /usr/local/... with sudo for a system-wide install; ~/.local/bin must be on PATH)
EOF

tarball taix-x86_64-linux-musl taix LICENSE README.md
tarball taix-gui-x86_64-linux-gnu taix-gui LICENSE dev.taix.TaiX.desktop dev.taix.TaiX.svg dev.taix.TaiX.png

for p in taix taix-gui; do
	sed -e "s|\${STAGE}|$STAGE|g" -e "s|\${VERSION}|$VERSION|g" "$root/packaging/nfpm-$p.yaml" > "$work/$p.yaml"
	nfpm package -f "$work/$p.yaml" -p deb -t "$out/${p}_${VERSION}_amd64.deb"
	nfpm package -f "$work/$p.yaml" -p rpm -t "$out/$p-$VERSION-1.x86_64.rpm"
	nfpm package -f "$work/$p.yaml" -p archlinux -t "$out/$p-$VERSION-1-x86_64.pkg.tar.zst"
done

(cd "$out" && rm -f SHA256SUMS && sha256sum *.tar.gz *.deb *.rpm *.pkg.tar.zst > SHA256SUMS)
echo "wrote $(wc -l < "$out/SHA256SUMS") artifacts + SHA256SUMS to $out"
