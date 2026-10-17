#!/usr/bin/env bash
# Prepares Ubuntu 24.04 for building and running the AV Relay Linux client
# from source: build dependencies, the udev rule that lets the desktop user
# create the virtual camera, and the v4l2loopback module (without its
# default dummy camera), now and at every boot. The .deb does the runtime
# part of this itself.
set -euo pipefail

"$(dirname "$0")/install-build-deps.sh"
sudo apt-get install -y v4l-utils v4l2loopback-utils

echo 'KERNEL=="v4l2loopback", SUBSYSTEM=="misc", TAG+="uaccess"' |
	sudo tee /etc/udev/rules.d/70-av-relay.rules >/dev/null
sudo udevadm control --reload

# Load it at every boot too, without the dummy camera.
echo v4l2loopback | sudo tee /etc/modules-load.d/av-relay.conf >/dev/null
echo 'options v4l2loopback devices=0' | sudo tee /etc/modprobe.d/av-relay.conf >/dev/null
if [[ ! -e /dev/v4l2loopback ]]; then
	sudo modprobe v4l2loopback devices=0
fi
sudo udevadm trigger --action=change /dev/v4l2loopback
echo "Done: v4l2loopback $(cat /sys/module/v4l2loopback/version)"
