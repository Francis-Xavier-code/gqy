#!/usr/bin/env bash
# 生成/更新 Homebrew formula（在 CNB Linux 构建容器内运行）
#
# 计算两个 macOS 资产的 sha256，回填到 packaging/brew/gqy.rb，
# 并将 CNB Release 的下载 URL 写入 formula。用法：
#   scripts/update-formula.sh <tag> <arm64_sha256> <x86_64_sha256>
set -euo pipefail

TAG="$1"
ARM_SHA="${2:-}"
X86_SHA="${3:-}"

FORMULA="packaging/brew/gqy.rb"
VERSION="${TAG#v}"

# 回填占位 sha256（默认全 0 → 用户后续手动填或 CI 替换）
arm_sha="${ARM_SHA:-0000000000000000000000000000000000000000000000000000000000000000}"
x86_sha="${X86_SHA:-$arm_sha}"

# 该公式只保留 arm64 条目（Homebrew 单一 arch 模板）；x86_64 资产仅随 Release 发布。
sed -i \
  -e "s#v0\.1\.0#${TAG}#g" \
  -e "s#0\.1\.0#${VERSION}#g" \
  -e "s#0000000000000000000000000000000000000000000000000000000000000000#${arm_sha}#" \
  "${FORMULA}"

echo "formula updated: ${TAG} (arm64 sha ${arm_sha:0:16}...)"
