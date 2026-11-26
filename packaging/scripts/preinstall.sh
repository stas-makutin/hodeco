#!/bin/sh
# Debian preinst / RPM %pre.
#
# Stop a running unit before the package replaces the binary and unit files.
# The marker tells postinstall whether the unit was active before the upgrade,
# so an administrator who stopped the service does not get it started again.
set -u

SERVICE="hodeco.service"
UNIT_FILE="/usr/lib/systemd/system/hodeco.service"
MARKER="/run/hodeco-package-was-active"
FIRST_UNIT_MARKER="/run/hodeco-package-first-unit"

rm -f "$MARKER" "$FIRST_UNIT_MARKER"

# Packages before the packaged systemd unit did not ship this file. Remember
# that so postinstall can enable and start the newly introduced service during
# the first upgrade, instead of treating it like a routine upgrade.
if [ ! -e "$UNIT_FILE" ]; then
    : > "$FIRST_UNIT_MARKER" 2>/dev/null || true
fi

if command -v systemctl >/dev/null 2>&1 && systemctl is-active --quiet "$SERVICE"; then
    : > "$MARKER" 2>/dev/null || true
    systemctl stop "$SERVICE" >/dev/null 2>&1 || true
fi

exit 0
