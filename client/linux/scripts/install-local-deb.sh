#!/usr/bin/env bash
# Installs the .deb built from this tree (cargo deb) on a machine that was
# set up for development with setup-dev.sh: removes that script's copies of
# the module and udev settings first, so dpkg doesn't ask about them (the
# package installs its own), and stops a development copy that's running.
set -euo pipefail
cd "$(dirname "$0")/.."

deb=$(ls -t target/debian/av-relay_*.deb 2>/dev/null | head -1)
[[ -n $deb ]] || { echo "No .deb in target/debian; run: cargo deb" >&2; exit 1; }

pkill -TERM -f 'target/(debug|release)/av-relay' 2>/dev/null || true
sudo rm -f /etc/modules-load.d/av-relay.conf /etc/modprobe.d/av-relay.conf /etc/udev/rules.d/70-av-relay.rules
# --reinstall: a rebuild keeps the version number.
sudo apt-get install -y --reinstall "./$deb"
echo "Installed $(basename "$deb"). Start AV Relay from the app grid (it also starts at every login)."
