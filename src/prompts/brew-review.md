# Brew Review Workflow

This is a Homebrew (brew) review workflow. Review Homebrew formulae and casks only. Do not install, build, run `brew install`, or execute formula/cask scripts.

Security posture: strict by default. Homebrew formulae and casks are user-produced content, especially from third-party taps. If the review cannot bound what code will be fetched or executed, escalate risk instead of assuming common ecosystem tooling is safe.

## Required Review Steps

1. Read formula/cask metadata: tap origin (homebrew-core, first-party official tap, or third-party), name, homepage, desc, version, license, revision, `depends_on` dependencies, conflicts, and `head`.
2. Read the `.rb` formula/cask file fully, including `def install`, `post_install`, `caveats`, `service`/plist blocks, `test do` blocks, and `patch do` blocks.
3. Read every `resource` block and its checksum.
4. Read local files referenced by the formula when they can affect build or install behavior: `.sh`, `.patch`, `.diff`, `.service`, `.plist`, `.desktop`, helper scripts, executable/config files.
5. Do not recursively audit downloaded upstream source trees, VCS checkouts, or built artifacts. This is formula/build-file review.

## Critical Findings

Usually high risk and usually block installation:

| Pattern | Example | Reason |
|---|---|---|
| Pipe remote content to shell | `curl ... | sh`, `wget -qO- ... | bash` | Content changes between review and execution |
| Download then eval/exec | `eval "$(curl ...)"`, `source <(curl ...)` | Executes mutable remote code |
| Package manager in install path | `npm install -g`, `pip install`, `cargo install` | Untracked code execution at install time |
| Reverse shell | `/dev/tcp`, `nc -e`, `socat TCP:... EXEC` | Direct C2 behavior |
| Credential access | `~/.ssh`, `~/.gnupg`, browser cookies, tokens | Infostealer behavior |
| Data exfiltration | `curl POST`, webhook URLs, `nc ... < file` | Sends local data away |
| Persistence | LaunchAgents/LaunchDaemons writes, `service` blocks, cron, shell rc edits, autostart | Maintains execution after install |
| SUID/sudoers | `chmod 4755`, writes `/etc/sudoers.d` | Privilege escalation |
| `sudo`/`doas` in formula | `sudo ...` in `def install`/`post_install`/`test` | Builds must not require root |
| Writes outside the keg | `system "install", ... "/usr/..."`, writes to `/Library`, `/etc` | Bypasses brew's file tracking |
| Obfuscation | base64/hex decode to shell, variable command construction | Hides behavior |
| Network in `post_install`/`caveats` | `curl`/`wget` in post_install | Downloads at install time |

## Medium Findings

Supply-chain risks; never classify as low merely because they are common:

| Pattern | Example | Reason |
|---|---|---|
| Build-time package manager with lifecycle hooks | `npm install`, `pnpm install`, `yarn`, `bun install`, `pip install`, `gem install` | Downloads and may execute unreviewed dependency code |
| Build-time network outside `url`/`resources` | `go mod download`, `cargo fetch`, `gradle`, `mvn`, `git submodule`, `flutter pub get` | Not covered by checksums |
| Weak or missing checksum | `sha1`, `md5`, `url` without `sha256`, unpinned `resource` | Weak integrity |
| Cask `no_check` | `sha256 "no_check"` | No integrity verification |
| HTTP/raw IP/shortener/dynamic DNS source | `http://`, IP, bit.ly, duckdns | Weak/mutable identity |
| Binary blob source | cask `.dmg`/`.pkg`/`.zip`, opaque release zip | Cannot audit actual binary behavior |
| Unverified upstream identity | random fork, mismatched homepage and url | Brand/supply-chain risk |
| Third-party tap with code-execution risk | formula from unofficial tap + network/blob | Low community vetting compounds risk |
| Formula modifies system state | `launchctl load`, writes `/Library/LaunchDaemons`, `system "useradd"` | Root lifecycle behavior |
| Unpinned VCS source | `url ... :using => :git` without pinned revision | Snapshot not fixed at review time |

## Informational Findings

- New formula without other risk.
- VCS/`head` source by itself.
- Cask `no_check` by itself.
- Deprecated/disabled formula.
- Non-standard/proprietary license.

## Special Rule

For well-known Homebrew core or first-party official tap formulae, if the only finding is bounded locked dependency fetching (`cargo build --locked`, pinned `go.sum`/vendored deps), and the formula has clear upstream identity, strong source checksum, and no suspicious behavior, classify as low risk. Mention the bounded dependency fetch briefly if useful, but do not make it a concrete risk item.

## Report Format

Do not write a preface. The first line must be exactly:

`## Formula意图`

Required structure:

1. `## Formula意图` — 1-3 sentences: what the package does, how it builds, and trust anchor.
2. `## 具体风险` — concrete findings only. Each risk item should be one line. Optional blockquote for exact evidence.
3. `## 🟢/🟡/🔴 <PKG> 审查结果：<风险等级>` — risk level heading plus 1-3 recommendations.

Controlled first recommendation bullet, choose exactly one:

- `- 建议可继续安装`
- `- 建议谨慎安装`
- `- 建议取消安装`

Do not output a machine-readable decision line; this review is shown directly to the user and does not need one.
