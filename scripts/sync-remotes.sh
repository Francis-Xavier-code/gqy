#!/usr/bin/env bash
# GQY 三平台仓库同步脚本（CNB / GitHub / Gitee）
#
# 本仓库同时镜像在三个代码托管平台：
#   origin  = CNB    https://cnb.cool/xynrin.ptt/GQY.git
#   github  = GitHub https://github.com/Francis-Xavier-code/gqy.git
#   gitee   = Gitee  https://gitee.com/Xynrin/GQY.git
#
# 用途：本地一键 fetch 三个远端并显示分歧，避免各自 clone、手动比对。
# 用法：
#   bash scripts/sync-remotes.sh           # fetch 全部 + 显示分歧摘要
#   bash scripts/sync-remotes.sh pull      # fetch 后把 main 拉到本地并合并
#   bash scripts/sync-remotes.sh push      # 把本地 main 推送到三个远端
#   bash scripts/sync-remotes.sh tags      # 对比三平台 tag 差异
#
# 提示：三个平台历史已存在分歧，pull/push 前请先跑（不带参数）查看摘要，
#       确认以哪个平台的 main 为准再做合并或推送。
set -euo pipefail

ORIGIN="origin"          # CNB
GH="github"
GT="gitee"
BRANCH="${SYNC_BRANCH:-main}"

log()  { printf '\033[1;34m%s\033[0m\n' "$*"; }
warn() { printf '\033[1;33m%s\033[0m\n' "$*"; }

# 确保三个 remote 都存在
ensure_remote() {
  local name="$1" url="$2"
  if ! git remote | grep -qx "$name"; then
    git remote add "$name" "$url"
    log "→ 已添加 remote $name → $url"
  fi
}
ensure_remote "$GH" "https://github.com/Francis-Xavier-code/gqy.git"
ensure_remote "$GT" "https://gitee.com/Xynrin/GQY.git"

fetch_all() {
  log "== 正在 fetch 三个远端 =="
  git fetch "$ORIGIN" 2>&1 | sed 's/^/  [CNB]   /'
  git fetch "$GH"     2>&1 | sed 's/^/  [GitHub] /' || warn "  GitHub fetch 失败（网络/认证），已跳过"
  git fetch "$GT"     2>&1 | sed 's/^/  [Gitee]  /' || warn "  Gitee fetch 失败（网络/认证），已跳过"
}

show_status() {
  log "== 各平台 main 指向 =="
  printf '  %-10s %s\n' "CNB:"   "$(git rev-parse --short "$ORIGIN/$BRANCH" 2>/dev/null || echo '?')"
  printf '  %-10s %s\n' "GitHub:" "$(git rev-parse --short "$GH/$BRANCH" 2>/dev/null || echo '?')"
  printf '  %-10s %s\n' "Gitee:"  "$(git rev-parse --short "$GT/$BRANCH" 2>/dev/null || echo '?')"

  log "== 相对本地 main 的分歧（领先/落后）=="
  for name in "$ORIGIN" "$GH" "$GT"; do
    local ref="$name/$BRANCH"
    if git rev-parse -q --verify "$ref" >/dev/null; then
      local ahead behind
      ahead=$(git rev-list --count "$ref..HEAD" 2>/dev/null || echo '?')
      behind=$(git rev-list --count "HEAD..$ref" 2>/dev/null || echo '?')
      printf '  %-10s 本地领先 %-4s  本地落后 %-4s\n' "$name" "$ahead" "$behind"
    fi
  done
}

show_tags() {
  log "== 各平台 tag 数量 =="
  for name in "$ORIGIN" "$GH" "$GT"; do
    local n
    n=$(git ls-remote --tags "$name" 2>/dev/null | grep -cv '\^{}' || true)
    printf '  %-10s %s 个 tag\n' "$name" "$n"
  done
}

case "${1:-status}" in
  pull)
    fetch_all
    show_status
    log "== 拉取并合并 $ORIGIN/$BRANCH（以 CNB 为准）=="
    git pull --ff-only "$ORIGIN" "$BRANCH" \
      || warn "  CNB main 无法 fast-forward（本地有 CNB 之外的新提交）。若要以某个远端为准，请先手动合并。"
    ;;
  push)
    log "== 推送本地 main 到三个远端 =="
    git push "$ORIGIN" "HEAD:$BRANCH" 2>&1 | sed 's/^/  [CNB]   /'
    git push "$GH"     "HEAD:$BRANCH" 2>&1 | sed 's/^/  [GitHub] /' || warn "  GitHub 推送失败（认证/权限）"
    git push "$GT"     "HEAD:$BRANCH" 2>&1 | sed 's/^/  [Gitee]  /' || warn "  Gitee 推送失败（认证/权限）"
    ;;
  tags)
    fetch_all
    show_tags
    ;;
  *)
    fetch_all
    show_status
    ;;
esac
