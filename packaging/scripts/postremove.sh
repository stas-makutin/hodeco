#!/bin/sh
# Debian postrm / RPM %postun.
#
# Refresh systemd after the unit file is gone. User accounts and data in
# /var/lib/hodeco and /var/log/hodeco are deliberately kept.
set -u

SERVICE="hodeco.service"

case "${1:-}" in
    0|remove|purge)
        rm -f /run/hodeco-package-was-active /run/hodeco-package-first-unit
        if command -v systemctl >/dev/null 2>&1; then
            systemctl daemon-reload >/dev/null 2>&1 || true
        fi
        ;;
esac

exit 0
