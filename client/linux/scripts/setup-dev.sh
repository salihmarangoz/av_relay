#!/usr/bin/env bash
# Prepares Ubuntu 24.04 for building and running the AV Relay Linux client
# from source: build dependencies, the udev rule that lets the desktop user
# create the virtual camera, and the v4l2loopback module (without its
# default dummy camera). The .deb does the runtime part of this itself.
set -euo pipefail

sudo apt-get install -y \
	build-essential pkg-config clang libclang-dev \
	libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libgstreamer-plugins-bad1.0-dev \
	gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav gstreamer1.0-nice \
	libpipewire-0.3-dev libspa-0.2-dev \
	libgtk-4-dev libadwaita-1-dev \
	v4l-utils v4l2loopback-utils

echo 'KERNEL=="v4l2loopback", SUBSYSTEM=="misc", TAG+="uaccess"' |
	sudo tee /etc/udev/rules.d/70-av-relay.rules >/dev/null
sudo udevadm control --reload

if [[ ! -e /dev/v4l2loopback ]]; then
	sudo modprobe v4l2loopback devices=0
fi
sudo udevadm trigger --action=change /dev/v4l2loopback
echo "Done: v4l2loopback $(cat /sys/module/v4l2loopback/version)"
