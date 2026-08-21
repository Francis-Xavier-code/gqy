#!/usr/bin/env bash
# GQY Linux 交叉编译辅助脚本（在 CNB Linux 构建容器内运行）
#
# 用法：
#   scripts/build-linux.sh <aarch64-unknown-linux-musl|x86_64-unknown-linux-musl>
#
# 在 Linux 容器中用 zig 交叉编译出静态 musl 二进制，产物位于
# target/<triple>/release/gqy。musl 静态二进制可移植到任意 Linux 发行版。
set -euo pipefail

TARGET="$1"
case "$TARGET" in
  aarch64-unknown-linux-musl) ;;
  x86_64-unknown-linux-musl)  ;;
  *) echo "unknown target: $TARGET" >&2; exit 1 ;;
esac

# 静态链接 alsa（cpal 的 Linux 后端）。zig 自带 musl libc，无需额外 sysroot；
# alsa-lib 静态库由 .cnb/Dockerfile 预编译到 /opt/alsa-lib/<triple>，
# 其 alsa.pc 中的路径已是绝对路径，故只需附加 PKG_CONFIG_PATH。
export ALSA_STATIC=true
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_PATH="/opt/alsa-lib/${TARGET}/lib/pkgconfig:${PKG_CONFIG_PATH:-}"

exec cargo zigbuild --target "$TARGET" --release
