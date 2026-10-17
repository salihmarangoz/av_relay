#!/usr/bin/env bash
# Installs what building the Linux client needs on Ubuntu 24.04 (used by
# setup-dev.sh and CI). Rust itself comes from rustup: edition 2024 needs a
# newer rustc than Ubuntu's.
set -euo pipefail
sudo apt-get install -y --no-install-recommends \
	build-essential pkg-config clang libclang-dev dpkg-dev \
	libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libgstreamer-plugins-bad1.0-dev \
	gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav gstreamer1.0-nice \
	libpipewire-0.3-dev libspa-0.2-dev \
	libgtk-4-dev libadwaita-1-dev
