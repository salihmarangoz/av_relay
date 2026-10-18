#!/usr/bin/env bash
# Installs the .deb built from this tree (cargo deb) on a machine that was
# set up for development with setup-dev.sh: removes that script's copies of
# the module and udev settings before the first install, so dpkg doesn't ask
# about them (the package installs its own), and stops a development copy
# that's running.
set -euo pipefail
cd "$(dirname "$0")/.."

deb=$(ls -t target/debian/av-relay_*.deb 2>/dev/null | head -1)
[[ -n $deb ]] || { echo "No .deb in target/debian; run: cargo deb" >&2; exit 1; }

pkill -TERM -f 'target/(debug|release)/av-relay' 2>/dev/null || true
sudo rm -f /etc/udev/rules.d/70-av-relay.rules
# Once the package is installed, the module settings are its own conffiles:
# dpkg never puts back a deleted conffile, so the module wouldn't load after
# the next restart.
if ! dpkg-query -W -f='${Status}' av-relay 2>/dev/null | grep -q 'ok installed'; then
	sudo rm -f /etc/modules-load.d/av-relay.conf /etc/modprobe.d/av-relay.conf
fi
# --reinstall: a rebuild keeps the version number. --force-confmiss restores
# the module settings that older copies of this script deleted.
sudo apt-get install -y --reinstall -o Dpkg::Options::=--force-confmiss "./$deb"
echo "Installed $(basename "$deb"). Start AV Relay from the app grid (it also starts at every login)."
