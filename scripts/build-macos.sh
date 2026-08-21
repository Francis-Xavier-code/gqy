#!/usr/bin/env bash
# GQY macOS 交叉编译辅助脚本（在 CNB Linux 构建容器内运行）
#
# 用法：
#   scripts/build-macos.sh <aarch64-apple-darwin|x86_64-apple-darwin>
#
# 在 Linux 容器中用 zig 交叉编译出 macOS 二进制，产物位于
# target/<triple>/release/gqy。
set -euo pipefail

TARGET="$1"
case "$TARGET" in
  aarch64-apple-darwin) CLANG_TARGET=aarch64-apple-macosx ;;
  x86_64-apple-darwin)  CLANG_TARGET=x86_64-apple-macosx ;;
  *) echo "unknown target: $TARGET" >&2; exit 1 ;;
esac

MACOS_SDK="${MACOS_SDK:-/opt/macos-sdk}"
LIBCLANG_PATH="${LIBCLANG_PATH:-/usr/lib/llvm-16/lib}"

export LIBCLANG_PATH
export COREAUDIO_SDK_PATH="$MACOS_SDK"
export SDKROOT="$MACOS_SDK"
export BINDGEN_EXTRA_CLANG_ARGS="--target=${CLANG_TARGET} -isysroot ${MACOS_SDK} -F ${MACOS_SDK}/System/Library/Frameworks -I ${MACOS_SDK}/usr/include"

cargo zigbuild --target "$TARGET" --release
