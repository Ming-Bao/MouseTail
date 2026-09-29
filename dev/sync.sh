#!/bin/sh
# Copy the workspace to the iMac for building there.
# The Linux machine to test against (an SSH host name or alias).
REMOTE=${MOUSETAIL_REMOTE:-omarchy}
set -e
cd "$(dirname "$0")/.."
rsync -az --delete --exclude target --exclude .git ./ "$REMOTE":~/Workspace/MouseTail/
