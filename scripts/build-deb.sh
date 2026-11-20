#!/usr/bin/env bash
# Build target/debian/limitbar_<version>_<arch>.deb from a release build.
# Needs: cargo, dpkg-deb, gzip. Usage: scripts/build-deb.sh
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
arch=$(dpkg --print-architecture)
cargo build --release --locked

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
chmod 755 "$stage"
install -Dm755 target/release/limitbar "$stage/usr/bin/limitbar"
install -Dm644 README.md "$stage/usr/share/doc/limitbar/README.md"
{
    echo "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/"
    echo "Upstream-Name: limitbar"
    echo
    echo "Files: *"
    echo "License: MIT"
    sed 's/^$/./; s/^/ /' LICENSE
} > "$stage/usr/share/doc/limitbar/copyright"
chmod 644 "$stage/usr/share/doc/limitbar/copyright"

# The binary needs glibc 2.39 or newer (Ubuntu 24.04+, Debian 13+).
mkdir -p "$stage/DEBIAN"
cat > "$stage/DEBIAN/control" <<EOF
Package: limitbar
Version: ${version/-/\~}
Architecture: $arch
Maintainer: Yasin Karatoprak
Depends: libc6 (>= 2.39), libgcc-s1, ca-certificates
Section: utils
Priority: optional
Description: AI usage limits in the terminal
 Shows session and weekly limits for Codex, Claude, Antigravity and Gemini,
 reading the sign-ins those tools already keep on this machine.
EOF

mkdir -p target/debian
out="target/debian/limitbar_${version/-/\~}_${arch}.deb"
dpkg-deb --root-owner-group --build "$stage" "$out" >/dev/null
echo "$out"
