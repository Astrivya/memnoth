#!/bin/sh
# Prepare the persistent volume, then exec the server as root.
#
# The container is sandboxed via compose: cap_drop: ALL, read_only: true,
# no-new-privileges: true, tmpfs /tmp. Running as root inside the container is
# required because the named volume is owned by root and chowning it would need
# CAP_CHOWN (dropped). The process does not gain anything beyond writing /data.
set -eu
mkdir -p /data/.memnoth
exec /usr/local/bin/memnoth "$@"
