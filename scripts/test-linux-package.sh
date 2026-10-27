#!/usr/bin/env bash
# Run as root inside an ephemeral distro container with /packages mounted read-only.
set -euo pipefail
trap 'echo "Package check failed at line $LINENO: $BASH_COMMAND" >&2' ERR
shopt -s nullglob

case "$PACKAGE_FORMAT" in
  deb)
    packages=(/packages/*.deb)
    test "${#packages[@]}" -eq 1
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    # Minimal images exclude documentation; keep ours so the license is checked.
    apt-get install --no-install-recommends -y \
      -o 'Dpkg::Options::=--path-include=/usr/share/doc/parakeetx/*' "${packages[0]}"
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
    # Override the base image's documentation exclusion for this package only.
    printf '\n[options]\nNoExtract = !usr/share/doc/parakeetx !usr/share/doc/parakeetx/*\n' >> /etc/pacman.conf
    install_log=$(mktemp)
    # Reinstallation also checks directories created by the first install.
    for _ in 1 2; do
      LC_ALL=C pacman -U --noconfirm "${packages[0]}" 2>&1 | tee -a "$install_log"
    done
    if grep -Fq 'directory permissions differ' "$install_log"; then
      echo 'Arch package contains incompatible directory modes' >&2
      exit 1
    fi
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
