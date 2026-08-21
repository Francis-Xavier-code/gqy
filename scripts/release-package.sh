#!/usr/bin/env bash
# GQY Release 打包脚本（在 CNB Linux 构建容器内运行，由 tag_push 发布流水线调用）
#
# 将构建好的 macOS（aarch64/x86_64）与 Linux（musl aarch64/x86_64）二进制
# 打包为 gqy-<tag>-<triple>.tar.gz，并回写 Homebrew formula 的 sha256。
#
# 注意：必须用 bash 执行（含 pipefail）。用法：
#   bash scripts/release-package.sh
set -euo pipefail

TAG="${CNB_BRANCH}"
ARM_TAR="gqy-${TAG}-aarch64-apple-darwin.tar.gz"
X86_TAR="gqy-${TAG}-x86_64-apple-darwin.tar.gz"

bash scripts/package-macos.sh "$TAG" aarch64-apple-darwin target/aarch64-apple-darwin/release/gqy
bash scripts/package-macos.sh "$TAG" x86_64-apple-darwin target/x86_64-apple-darwin/release/gqy

ARM_SHA="$(sha256sum "${ARM_TAR}" | cut -d' ' -f1)"
X86_SHA="$(sha256sum "${X86_TAR}" | cut -d' ' -f1)"
echo "arm64 sha: ${ARM_SHA}"
echo "x86_64 sha: ${X86_SHA}"

# 打包 Linux 双架构静态资产（x86_64 + aarch64 musl）
LINUX_X86="gqy-${TAG}-x86_64-unknown-linux-musl.tar.gz"
LINUX_ARM="gqy-${TAG}-aarch64-unknown-linux-musl.tar.gz"
bash scripts/package-linux.sh "$TAG" x86_64-unknown-linux-musl target/x86_64-unknown-linux-musl/release/gqy
bash scripts/package-linux.sh "$TAG" aarch64-unknown-linux-musl target/aarch64-unknown-linux-musl/release/gqy
echo "linux x86_64 sha: $(sha256sum "${LINUX_X86}" | cut -d' ' -f1)"
echo "linux aarch64 sha: $(sha256sum "${LINUX_ARM}" | cut -d' ' -f1)"

# 生成回填 sha256 的 formula，并提交回仓库（tag_push 为可信事件，
# CNB_TOKEN 具备写权限）
bash scripts/update-formula.sh "$TAG" "$ARM_SHA" "$X86_SHA"
git add packaging/brew/gqy.rb
if ! git diff --cached --quiet; then
  git -c user.name="GQY CI" -c user.email="ci@cnb.cool" \
      commit -m "chore(brew): update formula to ${TAG}"
  git -c credential.helper="store --file=/dev/null" \
      push "https://cnb:${CNB_TOKEN}@cnb.cool/xynrin.ptt/GQY.git" \
      HEAD:"$CNB_BRANCH"
fi
echo "formula committed for ${TAG}"
