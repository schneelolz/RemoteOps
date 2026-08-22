#!/bin/sh
set -eu

if [ ! -s /config/authorized_keys ]; then
    echo "缺少 /config/authorized_keys" >&2
    exit 1
fi

install -d -m 0700 -o remoteops -g remoteops /home/remoteops/.ssh
install -m 0600 -o remoteops -g remoteops \
    /config/authorized_keys \
    /home/remoteops/.ssh/authorized_keys
passwd -d remoteops >/dev/null
ssh-keygen -A

exec /usr/sbin/sshd \
    -D \
    -e \
    -o PasswordAuthentication=no \
    -o KbdInteractiveAuthentication=no \
    -o PermitEmptyPasswords=no \
    -o KexAlgorithms=curve25519-sha256,diffie-hellman-group14-sha256
