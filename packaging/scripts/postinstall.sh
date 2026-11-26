#!/bin/sh
# Debian postinst / RPM %post.
#
# Creates or repairs the service account, runtime directories, and permissions,
# then enables and starts the unit on a fresh install or when the package first
# introduces it. On a routine upgrade the unit is restarted only when preinstall
# recorded that it was running.
set -u

SERVICE="hodeco.service"
SERVICE_USER="hodeco"
SERVICE_GROUP="hodeco"
MARKER="/run/hodeco-package-was-active"
FIRST_UNIT_MARKER="/run/hodeco-package-first-unit"
SYSUSERS_FILE="/usr/lib/sysusers.d/hodeco.conf"

action="${1:-}"
case "$action" in
    configure)
        if [ -n "${2:-}" ]; then
            action="upgrade"
        else
            action="install"
        fi
        ;;
    1)
        action="install"
        ;;
    2)
        action="upgrade"
        ;;
esac

fail() {
    echo "hodeco: $*" >&2
    exit 1
}

start_service() {
    if [ ! -d /run/systemd/system ]; then
        return
    fi
    if command -v invoke-rc.d >/dev/null 2>&1; then
        invoke-rc.d hodeco start >/dev/null 2>&1
        status=$?
        # 0 means started; 101 means policy-rc.d denied the action (for example
        # in a container), which must be honored.
        if [ "$status" -eq 0 ] || [ "$status" -eq 101 ]; then
            return
        fi
    fi
    if command -v systemctl >/dev/null 2>&1; then
        systemctl start "$SERVICE" >/dev/null 2>&1 || true
    fi
}

# Prefer the declarative sysusers definition; fall back to shadow-utils when
# systemd-sysusers is not available.
if [ -f "$SYSUSERS_FILE" ] && command -v systemd-sysusers >/dev/null 2>&1; then
    systemd-sysusers "$SYSUSERS_FILE" >/dev/null 2>&1 || true
fi

if ! getent group "$SERVICE_GROUP" >/dev/null 2>&1; then
    command -v groupadd >/dev/null 2>&1 ||
        fail "groupadd is required to create the '${SERVICE_GROUP}' group"
    groupadd --system "$SERVICE_GROUP" ||
        fail "failed to create group '${SERVICE_GROUP}'"
fi

if ! getent passwd "$SERVICE_USER" >/dev/null 2>&1; then
    command -v useradd >/dev/null 2>&1 ||
        fail "useradd is required to create the '${SERVICE_USER}' user"
    useradd --system --gid "$SERVICE_GROUP" --home-dir /var/lib/hodeco \
        --no-create-home --shell /usr/sbin/nologin \
        --comment "Hodeco service" "$SERVICE_USER" ||
        fail "failed to create user '${SERVICE_USER}'"
fi

# Serial adapters are normally reachable through one of these groups.
for group in dialout uucp plugdev; do
    if getent group "$group" >/dev/null 2>&1; then
        if command -v usermod >/dev/null 2>&1; then
            usermod -a -G "$group" "$SERVICE_USER" >/dev/null 2>&1 || true
        elif command -v gpasswd >/dev/null 2>&1; then
            gpasswd -a "$SERVICE_USER" "$group" >/dev/null 2>&1 || true
        fi
    fi
done

# Runtime state and logs. Data is intentionally kept on uninstall.
install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_GROUP" \
    /var/lib/hodeco /var/log/hodeco ||
    fail "failed to create the hodeco runtime directories"

# The UI writes configuration and catalogs back through the service, so the
# directory must be writable by the hodeco group.
install -d -m 0770 -o root -g "$SERVICE_GROUP" /etc/hodeco ||
    fail "failed to prepare /etc/hodeco"

if [ -f /etc/hodeco/config.yaml ]; then
    chown root:"$SERVICE_GROUP" /etc/hodeco/config.yaml 2>/dev/null || true
    chmod 0640 /etc/hodeco/config.yaml 2>/dev/null || true
fi

if [ -f /etc/systemd/system/hodeco.service ]; then
    echo "hodeco: warning: /etc/systemd/system/hodeco.service overrides the packaged unit; remove it to use the package-managed service." >&2
fi

if command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload >/dev/null 2>&1 || true

    if [ "$action" = "install" ] || [ -e "$FIRST_UNIT_MARKER" ]; then
        systemctl enable "$SERVICE" >/dev/null 2>&1 || true
        start_service
    elif [ "$action" = "upgrade" ] && [ -e "$MARKER" ]; then
        start_service
    fi
fi

rm -f "$MARKER" "$FIRST_UNIT_MARKER"
exit 0
