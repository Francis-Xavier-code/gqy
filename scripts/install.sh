#!/usr/bin/env bash
# GQY（顾清影）在线安装脚本
#
# 自动检测 CNB Release 上的最新版本，识别当前操作系统与 CPU 架构，
# 下载对应的预编译资产（macOS / Linux × aarch64 / x86_64）并安装。
#
# 用法（从 Release 公开资产在线安装，自动检测最新版）：
#   bash <(curl -fsSL https://cnb.cool/xynrin.ptt/GQY/-/releases/download/<tag>/install.sh)
# 本地运行（默认装到 /usr/local/bin）：
#   bash scripts/install.sh
#   PREFIX=~/.local bash scripts/install.sh       # 指定安装目录
#   GQY_VERSION=v0.1.0 bash scripts/install.sh    # 锁定指定版本
#   bash scripts/install.sh --help
#
# 环境变量：
#   PREFIX        安装目录（默认 /usr/local，二进制安装到 $PREFIX/bin）
#   GQY_VERSION   指定要安装的版本 tag（默认自动检测最新版）
#   GQY_REPO      仓库 slug（默认 xynrin.ptt/GQY）
#   GQY_HOST      CNB Web 地址（默认 https://cnb.cool）
#   GQY_SKIP_VERIFY  设为 1 跳过 sha256 校验（不推荐）
set -euo pipefail

GQY_HOST="${GQY_HOST:-https://cnb.cool}"
GQY_REPO="${GQY_REPO:-xynrin.ptt/GQY}"
GQY_VERSION="${GQY_VERSION:-}"
PREFIX="${PREFIX:-/usr/local}"

# ── 帮助 ────────────────────────────────────────────────
if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  sed -n '2,20p' "$0"
  exit 0
fi

# ── 彩色输出 ────────────────────────────────────────────
if [[ -t 1 ]]; then
  C_RED=$'\033[31m'; C_GREEN=$'\033[32m'; C_YELLOW=$'\033[33m'; C_CYAN=$'\033[36m'; C_BOLD=$'\033[1m'; C_OFF=$'\033[0m'
else
  C_RED=; C_GREEN=; C_YELLOW=; C_CYAN=; C_BOLD=; C_OFF=
fi
info()  { printf "%s[*]%s %s\n" "$C_CYAN" "$C_OFF" "$*"; }
ok()    { printf "%s[✓]%s %s\n" "$C_GREEN" "$C_OFF" "$*"; }
warn()  { printf "%s[!]%s %s\n" "$C_YELLOW" "$C_OFF" "$*" >&2; }
die()   { printf "%s[✗]%s %s\n" "$C_RED" "$C_OFF" "$*" >&2; exit 1; }

need()  { command -v "$1" >/dev/null 2>&1 || die "缺少依赖工具：$1（请先安装后再运行本脚本）"; }

# ── 依赖检查 ────────────────────────────────────────────
need curl
need tar
command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 \
  || die "缺少 sha256 校验工具（sha256sum 或 shasum）"

# ── 1. 检测操作系统 ─────────────────────────────────────
OS="$(uname -s)"
case "$OS" in
  Darwin)  OS_TRIPLE="apple-darwin" ;;
  Linux)   OS_TRIPLE="unknown-linux-musl" ;;
  *) die "暂不支持的操作系统：$OS（目前仅支持 macOS / Linux）" ;;
esac

# ── 2. 检测 CPU 架构 ────────────────────────────────────
ARCH="$(uname -m)"
case "$ARCH" in
  aarch64|arm64)      ARCH_TRIPLE="aarch64" ;;
  x86_64|amd64)       ARCH_TRIPLE="x86_64" ;;
  *) die "暂不支持的 CPU 架构：$ARCH（目前仅支持 aarch64 / x86_64）" ;;
esac

TRIPLE="${ARCH_TRIPLE}-${OS_TRIPLE}"

# ── 3. 确定版本（自动检测最新版） ───────────────────────
release_page="${GQY_HOST}/${GQY_REPO}/-/releases"
if [[ -z "$GQY_VERSION" ]]; then
  info "正在检测最新版本…（${release_page}）"
  GQY_VERSION="$(
    curl -fsSL "$release_page" \
      | grep -oE "releases/tag/v[0-9]+\.[0-9]+\.[0-9]+" \
      | head -n1 | sed 's#.*/##'
  )" || true
  [[ -n "$GQY_VERSION" ]] || die "未能从 ${release_page} 检测到最新版本；可手动指定：GQY_VERSION=vX.Y.Z"
  ok "已检测到最新版本：${GQY_VERSION}"
else
  info "使用指定版本：${GQY_VERSION}"
fi

# 去掉版本号前缀中的 v，用于文件名（资产命名 gqy-v0.1.0-<triple>.tar.gz）
VERSION_NO_V="${GQY_VERSION#v}"

# ── 4. 构造下载地址并下载 ───────────────────────────────
ASSET="gqy-${GQY_VERSION}-${TRIPLE}.tar.gz"
URL="${GQY_HOST}/${GQY_REPO}/-/releases/download/${GQY_VERSION}/${ASSET}"

info "系统：${OS}（${ARCH}）→ 目标：${TRIPLE}"
info "下载资产：${ASSET}"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT
TAR_FILE="${TMP_DIR}/${ASSET}"

curl -fsSL -o "$TAR_FILE" "$URL" || die "下载失败：${URL}（请确认该版本存在对应架构的资产）"
ok "下载完成（$(du -h "$TAR_FILE" | cut -f1)）"

# ── 5. 校验 sha256（可选） ──────────────────────────────
# 资产 sha256 仅能通过 CNB OpenAPI 获取（需鉴权）。设置 CNB_TOKEN 时走
# OpenAPI 校验；否则使用 curl 对下载做完整性校验并给出提示。
if [[ "${GQY_SKIP_VERIFY:-0}" != "1" ]]; then
  if command -v sha256sum >/dev/null 2>&1; then
    SHA256="$(sha256sum "$TAR_FILE" | awk '{print $1}')"
  else
    SHA256="$(shasum -a 256 "$TAR_FILE" | awk '{print $1}')"
  fi

  expected=""
  if [[ -n "${CNB_TOKEN:-}" ]]; then
    # 通过 OpenAPI 获取该资产的官方 sha256（releases 接口返回 assets[].hash_value）
    expected="$(
      curl -fsSL -H "Authorization: Bearer ${CNB_TOKEN}" \
        "https://api.cnb.cool/${GQY_REPO}/-/releases" 2>/dev/null \
        | grep -oE "\"hash_value\":\"[0-9a-f]{64}\"" | head -n1 | grep -oE '[0-9a-f]{64}'
    )" || true
  fi

  if [[ -n "$expected" ]]; then
    if [[ "$SHA256" != "$expected" ]]; then
      die "sha256 校验失败：本地 ${SHA256} ≠ 官方 ${expected}。已中止安装。"
    fi
    ok "sha256 校验通过"
  else
    warn "未提供 CNB_TOKEN，跳过 sha256 校验（如需校验请设置 CNB_TOKEN）"
  fi
fi

# ── 6. 解压并安装 ───────────────────────────────────────
BIN_DIR="${PREFIX}/bin"
info "解压并安装到 ${BIN_DIR}…"
mkdir -p "$BIN_DIR"
tar -xzf "$TAR_FILE" -C "$TMP_DIR"
PKG_DIR="${TMP_DIR}/gqy-${GQY_VERSION}-${TRIPLE}"

install -m 0755 "${PKG_DIR}/gqy" "${BIN_DIR}/gqy"

# 附带资源（memes 表情库等）安装到共享目录，便于运行时按相对路径解析
if [[ -d "${PKG_DIR}/memes" ]]; then
  SHARE_DIR="${PREFIX}/share/gqy"
  mkdir -p "$SHARE_DIR"
  cp -r "${PKG_DIR}/memes" "$SHARE_DIR/memes"
  ok "已安装资源：${SHARE_DIR}/memes"
fi

# ── 7. 完成 ─────────────────────────────────────────────
if ! echo ":$PATH:" | grep -q ":${BIN_DIR}:"; then
  warn "${BIN_DIR} 不在当前 PATH 中，请将其加入 PATH："
  warn "  export PATH=\"${BIN_DIR}:\$PATH\""
fi

ok "GQY ${GQY_VERSION} 安装完成 → ${BIN_DIR}/gqy"
command -v gqy >/dev/null 2>&1 && {
  echo
  info "运行 gqy 开始使用；或查看帮助：gqy help"
}
