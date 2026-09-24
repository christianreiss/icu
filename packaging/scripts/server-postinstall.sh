#!/bin/sh
# Not enabled or started: server.server_name in /etc/icu/server.toml must be
# set first.
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || :
    # Upgrade: pick up the new binary if the service is running.
    systemctl try-restart icu-server.service || :
fi
exit 0
