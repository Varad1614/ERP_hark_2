#!/bin/bash
# Container health check without curl/wget: GET /minio/health/live over bash's /dev/tcp.
exec 3<>/dev/tcp/127.0.0.1/9000 || exit 1
printf 'GET /minio/health/live HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n' >&3
IFS= read -r -t 3 status <&3 || exit 1
case "$status" in *" 200 "*) exit 0 ;; *) exit 1 ;; esac
