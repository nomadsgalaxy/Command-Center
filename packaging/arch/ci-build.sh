#!/bin/bash
# Builds the Arch package and its repo database, as root inside an Arch Linux (x86_64) or Arch
# Linux ARM (aarch64) container. CI runs it from .github/workflows/arch.yml:
#   packaging/arch/ci-build.sh <out dir>
# With CC_SIGNING_KEY (the armoured secret key) and CC_SIGNING_FPR it signs the package and the
# database (docs/packaging.md); without them everything comes out unsigned, for test builds.
set -euo pipefail
shopt -s extglob nullglob
here=$(realpath "$(dirname "$0")")
out=$(realpath -m "$1")

sed -i 's/^CheckSpace/#CheckSpace/' /etc/pacman.conf  # it can't read a container's mounts
pacman-key --init >/dev/null
if grep -q '^ID=archarm' /etc/os-release; then pacman-key --populate archlinuxarm; else pacman-key --populate archlinux; fi >/dev/null
deps=$(cd "$here" && startdir=$here && . ./PKGBUILD && echo "${depends[@]} ${makedepends[@]}")
pacman -Syu --noconfirm --needed base-devel $deps

# makepkg won't run as root. The PGP check is skipped because the PKGBUILD pins krdp's sha256.
id builder >/dev/null 2>&1 || useradd -m builder
chown -R builder "$here/../.."
su builder -c "cd '$here' && makepkg --noconfirm --skippgpcheck"

mkdir -p "$out"
cp "$here"/*.pkg.tar.@(zst|xz) "$out"
cd "$out"
pkgs=(*.pkg.tar.@(zst|xz))
if [ -n "${CC_SIGNING_KEY:-}" ]; then
  printf '%s\n' "$CC_SIGNING_KEY" | gpg --batch --quiet --import
  for p in "${pkgs[@]}"; do gpg --batch --yes --detach-sign -u "$CC_SIGNING_FPR" "$p"; done
  GPGKEY=$CC_SIGNING_FPR repo-add -q -s command-center.db.tar.gz "${pkgs[@]}"
else
  repo-add -q command-center.db.tar.gz "${pkgs[@]}"
fi
# repo-add makes command-center.db and .files as symlinks, which a release can't hold.
for f in command-center.db command-center.files; do
  cp --remove-destination "$f.tar.gz" "$f"
  if [ -f "$f.tar.gz.sig" ]; then cp --remove-destination "$f.tar.gz.sig" "$f.sig"; fi
done
ls -l
