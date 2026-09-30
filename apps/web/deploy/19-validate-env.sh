#!/bin/sh
set -eu
# API 地址同时进入脚本和 CSP；启动前拒绝路径、引号与配置注入字符。
if ! printf '%s' "$API_ORIGIN" | grep -Eq '^(https://[a-zA-Z0-9][a-zA-Z0-9.-]*|http://(localhost|127\.0\.0\.1))(:[0-9]{1,5})?$'; then
  echo 'API_ORIGIN 必须是无路径的 HTTPS 源，仅本地允许 HTTP' >&2
  exit 1
fi
case "$PORT" in
  ''|*[!0-9]*) echo 'PORT 必须是有效端口' >&2; exit 1 ;;
esac
if [ "$PORT" -lt 1 ] || [ "$PORT" -gt 65535 ]; then
  echo 'PORT 必须介于 1 和 65535' >&2
  exit 1
fi
