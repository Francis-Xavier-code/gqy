# GQY CNB 云端构建环境
#
# 提供三类能力：
#   1. Linux 原生 CI：cargo fmt / clippy / test（Rust stable + 系统依赖库）
#   2. macOS 交叉编译：cargo-zigbuild + zig 0.15.2 + macOS SDK 26.1
#      （在 Linux 容器内产出 aarch64-apple-darwin / x86_64-apple-darwin 二进制）
#   3. Linux 静态交叉编译：cargo-zigbuild + zig，产出 musl 静态二进制
#      （aarch64/x86_64-unknown-linux-musl，静态链接 alsa）
#
# 由 .cnb.yml 的 docker.build 引用，CNB 会自动缓存镜像层，仅当本文件变化时重建。

FROM rust:1.97-bookworm

# ── 系统依赖：构建 Linux 目标测试所需的 native 库 + 交叉编译工具 ──
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential pkg-config \
        libssl-dev libfontconfig1-dev libasound2-dev libudev-dev \
        clang-16 libclang-16-dev \
        xz-utils ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# ── zig（用于 macOS 交叉编译链接/编译 C 依赖）──
ARG ZIG_VERSION=0.15.2
ARG ZIG_SHA256=02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239
RUN curl -fsSL "https://ziglang.org/download/${ZIG_VERSION}/zig-x86_64-linux-${ZIG_VERSION}.tar.xz" -o /tmp/zig.tar.xz \
    && echo "${ZIG_SHA256}  /tmp/zig.tar.xz" | sha256sum -c - \
    && tar -xJf /tmp/zig.tar.xz -C /usr/local \
    && mv "/usr/local/zig-x86_64-linux-${ZIG_VERSION}" /usr/local/zig \
    && rm /tmp/zig.tar.xz \
    && ln -s /usr/local/zig/zig /usr/local/bin/zig

# ── CI 工具组件：rustfmt（cargo fmt）与 clippy ──
# rust:1.97-bookworm 基础镜像默认不含 rustfmt/clippy，缺失会导致
# `cargo fmt --check` / `cargo clippy` 报 "component not installed"。
RUN rustup component add rustfmt clippy

# ── cargo-zigbuild：让 cargo 直接走 zig 交叉编译 ──
RUN cargo install cargo-zigbuild --version 0.23.0 \
    && rustup target add aarch64-apple-darwin x86_64-apple-darwin \
    && rustup target add aarch64-unknown-linux-musl x86_64-unknown-linux-musl

# ── alsa-lib 静态库（cpal 的 Linux 后端依赖；供 musl 交叉编译链接）──
# cpal 在 Linux 上强制链接 alsa，交叉编译静态 musl 目标时需要提供对应架构的
# 静态 libasound.a。这里用 zig 分别交叉编译 aarch64/x86_64 的 alsa-lib，
# 安装到 /opt/alsa-lib/<triple> 供 build-linux.sh 通过 PKG_CONFIG 引用。
ARG ALSA_VERSION=1.2.13
RUN curl -fsSL "https://www.alsa-project.org/files/pub/lib/alsa-lib-${ALSA_VERSION}.tar.bz2" -o /tmp/alsa.tar.bz2 \
    && tar -xjf /tmp/alsa.tar.bz2 -C /tmp \
    && for triple in aarch64-unknown-linux-musl x86_64-unknown-linux-musl; do \
         mkdir -p "/tmp/alsa-build/${triple}" "/opt/alsa-lib/${triple}"; \
         cd "/tmp/alsa-build/${triple}"; \
         CC="zig cc -target ${triple}" \
         AR="zig ar" \
         /tmp/alsa-lib-${ALSA_VERSION}/configure \
           --host="${triple}" --disable-shared --enable-static \
           --with-pic --prefix="/opt/alsa-lib/${triple}" \
           >/dev/null; \
         make -j$(nproc) >/dev/null; \
         make install >/dev/null; \
       done \
    && rm -rf /tmp/alsa-lib-* /tmp/alsa-build /tmp/alsa.tar.bz2

# ── macOS SDK（交叉编译 bindgen/链接所需头文件与 tbd 库）──
# 解压后精简：仅保留编译 GQY 所需的系统 framework 与头文件，控制镜像体积。
ARG MACOS_SDK=/opt/macos-sdk
ARG SDK_URL=https://github.com/joseluisq/macosx-sdks/releases/download/26.1/MacOSX26.1.sdk.tar.xz
ARG SDK_SHA256=beee7212d265a6d2867d0236cc069314b38d5fb3486a6515734e76fa210c784c
RUN curl -fsSL "${SDK_URL}" -o /tmp/sdk.tar.xz \
    && echo "${SDK_SHA256}  /tmp/sdk.tar.xz" | sha256sum -c - \
    && mkdir -p /opt \
    && tar -xJf /tmp/sdk.tar.xz -C /opt \
    && mv /opt/MacOSX26.1.sdk "${MACOS_SDK}" \
    && rm /tmp/sdk.tar.xz

# 精简 SDK：删除私有框架与编译用不到的框架，仅保留核心系统框架。
# 注意：删除列表需与 src 依赖的 framework 一致（AudioUnit/CoreAudio/CoreAudioTypes
# 及它们的传递依赖）。若后续新增原生 framework 依赖，在此补充保留项。
RUN cd "${MACOS_SDK}/System/Library/Frameworks" \
    && rm -rf /opt/macos-sdk/System/Library/PrivateFrameworks \
    && ls | grep -v -E '^(AudioUnit|CoreAudio|CoreAudioTypes|CoreFoundation|Foundation|AudioToolbox|SystemConfiguration|Security|CoreServices|CFNetwork|ApplicationServices|CoreGraphics|CoreText|CoreMedia|CoreVideo|QuartzCore|Metal|OpenGL)\.framework$' \
       | while read f; do rm -rf "$f"; done

# 链接器/编译缓存目录（CNB 可通过 volumes 复用，加速多次构建）
ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    LIBCLANG_PATH=/usr/lib/llvm-16/lib \
    PATH="/usr/local/cargo/bin:/usr/local/zig:${PATH}"

# alsa 静态库允许在 musl 目标下用 pkg-config 交叉查找
ENV ALSA_STATIC=true \
    PKG_CONFIG_ALLOW_CROSS=1

# CNB 构建容器启用 userns（用户命名空间重映射），cargo 以非 root 用户运行；
# 显式放开 cargo/rustup 目录写权限，避免 clippy/test 阶段无法写 registry 缓存。
RUN chmod -R a+rwX /usr/local/cargo /usr/local/rustup \
    && mkdir -p /workspace/target && chmod -R a+rwX /workspace

WORKDIR /workspace
