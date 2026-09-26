#!/bin/sh
# Not enabled or started: check server.server_name in /etc/icu/server.toml
# first (unset, it is the host's FQDN, if that resolves to this host).
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || :
    # Upgrade: pick up the new binary if the service is running.
    systemctl try-restart icu-server.service || :
fi
exit 0
