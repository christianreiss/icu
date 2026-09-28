#!/bin/sh
# The group must exist before /etc/icu/server.toml (group icu) is unpacked.
# The same line ships as /usr/lib/sysusers.d/icu-server.conf for later boots.
printf 'g icu -\ng icu-monitor -\n' |
    systemd-sysusers --replace=/usr/lib/sysusers.d/icu-server.conf - || :
for group in icu icu-monitor; do
    getent group "$group" >/dev/null || groupadd --system "$group"
done
exit 0
