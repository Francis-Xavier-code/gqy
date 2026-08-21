#!/usr/bin/env bash
# GQY Release 打包脚本（在 CNB Linux 构建容器内运行）
#
# 将构建好的 Linux 二进制与资源打包为
# gqy-<tag>-<triple>.tar.gz，供 Release 附件上传。
#
# 用法：
#   scripts/package-linux.sh <tag> <triple> <release_binary_path>
set -euo pipefail

TAG="$1"
TRIPLE="$2"
BINARY="${3:-target/${TRIPLE}/release/gqy}"

if [ ! -x "$BINARY" ]; then
  echo "binary not found: $BINARY" >&2
  exit 1
fi

PKG="gqy-${TAG}-${TRIPLE}"
mkdir -p "dist/${PKG}"
cp "$BINARY" "dist/${PKG}/gqy"
cp -r src/memes "dist/${PKG}/memes"
cp -r src/scripts "dist/${PKG}/scripts"
# 随包附带说明，便于手动排查（可选）
cp packaging/README.md "dist/${PKG}/README.md" 2>/dev/null || true

tar -czf "${PKG}.tar.gz" -C dist "${PKG}"
echo "packaged: ${PKG}.tar.gz"
ls -lh "${PKG}.tar.gz"
