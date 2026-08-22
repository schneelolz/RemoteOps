FROM alpine:3.22

RUN apk add --no-cache openssh-server \
    && adduser -D -s /bin/sh remoteops \
    && install -d -m 0700 -o remoteops -g remoteops /home/remoteops/.ssh /config

COPY entrypoint.sh /usr/local/bin/remoteops-lab-ssh-entrypoint

RUN chmod 0755 /usr/local/bin/remoteops-lab-ssh-entrypoint

EXPOSE 22

HEALTHCHECK --interval=5s --timeout=3s --start-period=5s --retries=5 \
    CMD nc -z 127.0.0.1 22 || exit 1

ENTRYPOINT ["/usr/local/bin/remoteops-lab-ssh-entrypoint"]

