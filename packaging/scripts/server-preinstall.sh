#!/bin/sh
# The group must exist before /etc/icu/server.toml (group icu) is unpacked.
# The same line ships as /usr/lib/sysusers.d/icu-server.conf for later boots.
echo 'g icu -' | systemd-sysusers --replace=/usr/lib/sysusers.d/icu-server.conf - || :
getent group icu >/dev/null || groupadd --system icu
exit 0
