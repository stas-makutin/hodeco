#!/bin/sh
# Debian prerm / RPM %preun.
#
# Stop and disable the unit only on a real uninstall. During an upgrade,
# preinstall has already stopped it and postinstall will restart it.
set -u

SERVICE="hodeco.service"

case "${1:-}" in
    0|remove)
        if command -v systemctl >/dev/null 2>&1; then
            systemctl stop "$SERVICE" >/dev/null 2>&1 || true
            systemctl disable "$SERVICE" >/dev/null 2>&1 || true
        fi
        ;;
esac

exit 0
