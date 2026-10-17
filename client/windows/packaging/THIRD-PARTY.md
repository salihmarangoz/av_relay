# Bundled runtime and source

AV Relay is GPL-3.0-or-later; LICENSE is included. Its matching source and
build instructions are in the release tag at https://github.com/salihmarangoz/av_relay.

This installer dynamically loads an unmodified subset of the official
GStreamer MSVC x86_64 1.28.8 runtime. Component versions are listed in
share/versions.txt and their copyright and license texts in share/licenses.
GStreamer and related libraries remain ordinary replaceable DLL files.

The corresponding runtime source and build recipes are available from:

- https://gstreamer.freedesktop.org/src/ (GStreamer components)
- https://gitlab.freedesktop.org/gstreamer/cerbero/-/tree/1.28.8
  (dependency versions, patches, source URLs, and Windows build recipes)

The Cerbero tag identifies the dependency sources and patches used by the
official runtime. No modifications to those runtime libraries are made here.

Microsoft Visual C++ runtime DLLs are distributed under the Visual Studio
redistribution terms. Windows supplies its own system DLLs.

OBS Studio and VB-CABLE are separate installations and are not redistributed.
