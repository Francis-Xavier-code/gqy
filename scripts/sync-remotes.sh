#!/usr/bin/env bash
# GQY 多平台仓库同步脚本，以 CNB 为主
#
# 只使用仓库里已存在的 remote（本仓库：cnb=CNB, origin=GitHub, 可选 gitee），
# 不新增/修改任何 git 配置（user.name、user.email、remote 均不动）。
#
# 用法：
#   bash scripts/sync-remotes.sh           # fetch 全部 + 显示分歧摘要
#   bash scripts/sync-remotes.sh pull      # fetch 后把本地 main 快进到 cnb/main
#   bash scripts/sync-remotes.sh push      # 把本地 main 推送到所有 remote（cnb 优先）
#   bash scripts/sync-remotes.sh tags      # 对比各平台 tag 差异
set -euo pipefail

CNB="cnb"               # 主 remote
BRANCH="${SYNC_BRANCH:-main}"

log()  { printf '\033[1;34m%s\033[0m\n' "$*"; }
warn() { printf '\033[1;33m%s\033[0m\n' "$*"; }

# 仓库中实际存在的 remote（保序）
REMOTES=()
for r in $(git remote); do
  [ "$r" = "$CNB" ] && continue   # cnb 放最前
  REMOTES+=("$r")
done
ALL=("$CNB" "${REMOTES[@]}")

fetch_all() {
  log "== 正在 fetch 各 remote =="
  for r in "${ALL[@]}"; do
    git fetch "$r" 2>&1 | sed "s/^/  [$r] /" || warn "  $r fetch 失败（网络/认证），已跳过"
  done
}

show_status() {
  log "== 各平台 $BRANCH 指向 =="
  for r in "${ALL[@]}"; do
    printf '  %-10s %s\n' "$r:" "$(git rev-parse --short "$r/$BRANCH" 2>/dev/null || echo '?')"
  done

  log "== 相对本地 $BRANCH 的分歧（领先/落后）=="
  for r in "${ALL[@]}"; do
    local ref="$r/$BRANCH"
    if git rev-parse -q --verify "$ref" >/dev/null; then
      printf '  %-10s 本地领先 %-4s  本地落后 %-4s\n' \
        "$r" "$(git rev-list --count "$ref..HEAD" 2>/dev/null || echo '?')" \
             "$(git rev-list --count "HEAD..$ref" 2>/dev/null || echo '?')"
    fi
  done
}

show_tags() {
  log "== 各平台 tag 数量 =="
  for r in "${ALL[@]}"; do
    printf '  %-10s %s 个 tag\n' "$r" "$(git ls-remote --tags "$r" 2>/dev/null | grep -cv '\^{}' || true)"
  done
}

case "${1:-status}" in
  pull)
    fetch_all
    show_status
    log "== 快进到 $CNB/$BRANCH（以 CNB 为准）=="
    git pull --ff-only "$CNB" "$BRANCH" \
      || warn "  无法 fast-forward（本地有 CNB 之外的新提交）。若要以 CNB 为准，先手动解决。"
    ;;
  push)
    log "== 推送本地 $BRANCH 到各 remote（$CNB 优先）=="
    for r in "${ALL[@]}"; do
      git push "$r" "HEAD:$BRANCH" 2>&1 | sed "s/^/  [$r] /" \
        || warn "  $r 推送失败（认证/权限/分叉），已跳过"
    done
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
