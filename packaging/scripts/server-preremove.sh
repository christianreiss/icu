#!/bin/sh
# Only on real removal: rpm passes 0, dpkg passes "remove"; upgrades keep the
# service as it is. State in /var/lib/private/icu (ca.key!) is never touched.
case "$1" in
    0|remove)
        if [ -d /run/systemd/system ]; then
            systemctl disable --now icu-server.service || :
        fi
        ;;
esac
exit 0
