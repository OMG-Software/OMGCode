#!/usr/bin/env bash
# Install packages from the Ubuntu archive on a GitHub-hosted Ubuntu runner.
#
# apt reads only the image's Ubuntu source file, so a third-party repository
# that the runner image adds (such as packages.microsoft.com) cannot fail the
# step. Any error from the Ubuntu archive itself fails it, a transient one
# included, and package signatures are checked as usual.
set -euo pipefail

if [[ "$#" -eq 0 ]]; then
  echo "usage: $0 PACKAGE..." >&2
  exit 2
fi

ubuntu_sources=/etc/apt/sources.list.d/ubuntu.sources
if [[ ! -f "$ubuntu_sources" ]]; then
  echo "::error::$ubuntu_sources is missing; the runner image no longer has the expected Ubuntu source file." >&2
  ls -la /etc/apt /etc/apt/sources.list.d >&2 || true
  exit 1
fi

# apt picks the deb822 parser from the .sources extension. A source-parts
# directory of /dev/null reads no other file, and List-Cleanup=0 keeps the
# package lists of the sources skipped here. By default apt-get update only
# warns when an index fetch fails and keeps the image's older lists;
# --error-on=any makes that a failure.
apt_sources=(
  -o "Dir::Etc::SourceList=$ubuntu_sources"
  -o Dir::Etc::SourceParts=/dev/null
)
sudo apt-get "${apt_sources[@]}" -o APT::Get::List-Cleanup=0 --error-on=any update
sudo apt-get "${apt_sources[@]}" install -y --no-install-recommends "$@"
