#!/usr/bin/env bash
# Run as root inside an ephemeral distro container with /packages mounted read-only.
set -euo pipefail
shopt -s nullglob

case "$PACKAGE_FORMAT" in
  deb)
    packages=(/packages/*.deb)
    test "${#packages[@]}" -eq 1
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install --no-install-recommends -y "${packages[0]}"
    ;;
  rpm)
    packages=(/packages/*.rpm)
    test "${#packages[@]}" -eq 1
    dnf install --setopt=install_weak_deps=False -y "${packages[0]}"
    test "$(rpm -q --queryformat '%{LICENSE}' parakeetx)" = GPL-3.0-only
    ;;
  archlinux)
    packages=(/packages/*.pkg.tar.zst)
    test "${#packages[@]}" -eq 1
    pacman -Syu --noconfirm
    pacman -U --noconfirm "${packages[0]}"
    ;;
  *) echo "Unknown package format: $PACKAGE_FORMAT" >&2; exit 1 ;;
esac

test "$(parakeetx --version)" = "parakeetx $PACKAGE_VERSION"
parakeetx --help
test -L /usr/bin/parakeetx
test -f /usr/share/applications/app.parakeetx.desktop.desktop
test -f /usr/share/parakeetx/python/requirements.txt
test -f /usr/share/doc/parakeetx/LICENSE

# A package uninstall must preserve user data.
mkdir -p /root/.local/share/parakeetx
echo retained > /root/.local/share/parakeetx/package-test-marker
case "$PACKAGE_FORMAT" in
  deb) apt-get remove -y parakeetx ;;
  rpm) dnf remove -y parakeetx ;;
  archlinux) pacman -R --noconfirm parakeetx ;;
esac
test ! -e /usr/bin/parakeetx
test ! -L /usr/bin/parakeetx
test ! -e /usr/lib/parakeetx/parakeetx
test "$(cat /root/.local/share/parakeetx/package-test-marker)" = retained
