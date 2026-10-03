# syntax=docker/dockerfile:1
FROM node:24-bookworm-slim AS base
ENV PNPM_HOME=/pnpm
ENV PATH=$PNPM_HOME/bin:$PNPM_HOME:$PATH
RUN npm install --global pnpm@12.3.4
WORKDIR /app

FROM base AS build
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
RUN --mount=type=cache,id=pnpm-store,target=/pnpm/store pnpm install --frozen-lockfile
COPY tsconfig.json vite.config.ts index.html official.html ./
COPY src ./src
COPY shared ./shared
COPY public ./public
RUN pnpm build

FROM rust:1.97.1-bookworm AS backend
RUN apt-get update && apt-get install -y --no-install-recommends clang libclang-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY backend/Cargo.toml ./backend/Cargo.toml
COPY official-service/Cargo.toml ./official-service/Cargo.toml
# A separate dependency layer survives application edits in remote BuildKit
# caches. Cargo cache mounts alone do not persist on fresh GitHub runners.
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry \
    mkdir -p backend/src backend/examples official-service/src \
    && printf 'fn main() {}\n' > backend/src/main.rs \
    && printf 'fn main() {}\n' > backend/examples/ublk_probe.rs \
    && printf '' > backend/src/lib.rs \
    && printf 'fn main() {}\n' > official-service/src/main.rs \
    && printf '' > official-service/src/lib.rs && cargo build --locked --release --bin leo --features ublk
COPY backend ./backend
COPY deploy/nodes ./deploy/nodes
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry \
    touch backend/src/main.rs backend/src/lib.rs && \
    cargo build --locked --release --bin leo --features ublk && cp target/release/leo /usr/local/bin/leo

# The direct block backend shares guest RAM with Firecracker. Upstream 1.17.0
# does not reclaim MAP_SHARED memfd pages on balloon/free-page reporting.
# Keep the patch narrow, pinned and tested; retain the upstream seccomp policy.
FROM rust:1.97.0-bookworm AS firecracker
RUN apt-get update && apt-get install -y --no-install-recommends \
    musl-tools clang libclang-dev libseccomp-dev cmake && rm -rf /var/lib/apt/lists/* \
    && ln -s /usr/include/linux /usr/include/x86_64-linux-musl/linux \
    && ln -s /usr/include/asm-generic /usr/include/x86_64-linux-musl/asm-generic \
    && ln -s /usr/include/x86_64-linux-gnu/asm /usr/include/x86_64-linux-musl/asm
WORKDIR /firecracker
RUN curl -fsSL https://codeload.github.com/firecracker-microvm/firecracker/tar.gz/95f868c8e345b1cc8faccd1a3c910b4989dc3f58 -o /tmp/firecracker-source.tar.gz \
    && echo '27280787407229c412901dfcf74cd378f1336010e71050aba43ace51cd4a0f4f  /tmp/firecracker-source.tar.gz' | sha256sum -c - \
    && tar -xzf /tmp/firecracker-source.tar.gz --strip-components=1 \
    && rm /tmp/firecracker-source.tar.gz
COPY deploy/microvm/firecracker-shared-memory.patch /tmp/firecracker.patch
RUN git apply /tmp/firecracker.patch
RUN --mount=type=cache,id=cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=firecracker-git,target=/usr/local/cargo/git \
    cargo test --locked -p vmm --lib test_discard_range \
    && cargo build --locked --release --bin firecracker --target x86_64-unknown-linux-musl \
    && cp build/cargo_target/x86_64-unknown-linux-musl/release/firecracker /usr/local/bin/firecracker

FROM base AS runtime
ARG PLAYWRIGHT_VERSION=1.63.0
# OS libraries are shared by project-pinned browser versions in the persistent home.
# Keep this layer independent of application code and package versions.
RUN npm exec --yes --package="playwright@${PLAYWRIGHT_VERSION}" -- playwright install-deps chromium firefox webkit \
    && rm -rf /var/lib/apt/lists/* /root/.npm
# Resolve stable tool versions before building, then validate this exact image.
ARG GH_VERSION
ARG CODEX_VERSION
ARG CLAUDE_VERSION=2.1.280
ARG TARGETARCH
RUN test -n "$CODEX_VERSION" && test -n "$GH_VERSION"
ENV NODE_ENV=production HOST=0.0.0.0 PORT=4310 DATA_DIR=/data AGENT_HOME=/home/node WORKSPACE_ROOTS=/workspaces
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl wget git git-lfs openssh-client python3 awscli build-essential bubblewrap socat \
    zip unzip xz-utils zstd bzip2 rsync file less tree sqlite3 postgresql-client \
    dnsutils iproute2 iputils-ping netcat-openbsd procps lsof strace patch diffutils \
    pkg-config libssl-dev libffi-dev ninja-build poppler-utils imagemagick ffmpeg \
    && rm -rf /var/lib/apt/lists/*
RUN arch="${TARGETARCH:-amd64}" \
    && curl -fsSL "https://github.com/cli/cli/releases/download/v${GH_VERSION}/gh_${GH_VERSION}_linux_${arch}.tar.gz" -o /tmp/gh.tar.gz \
    && curl -fsSL "https://github.com/cli/cli/releases/download/v${GH_VERSION}/gh_${GH_VERSION}_checksums.txt" -o /tmp/gh-checksums.txt \
    && expected=$(awk -v name="gh_${GH_VERSION}_linux_${arch}.tar.gz" '$2 == name {print $1}' /tmp/gh-checksums.txt) \
    && test -n "$expected" && printf '%s  /tmp/gh.tar.gz\n' "$expected" | sha256sum --check - \
    && tar -xzf /tmp/gh.tar.gz -C /tmp \
    && cp "/tmp/gh_${GH_VERSION}_linux_${arch}/bin/gh" /usr/local/bin/gh \
    && rm -rf /tmp/gh*
# Claude's pinned postinstall places the native executable; pnpm otherwise skips it.
RUN pnpm add --global --allow-build @anthropic-ai/claude-code "@openai/codex@${CODEX_VERSION}" "@anthropic-ai/claude-code@${CLAUDE_VERSION}" \
    && claude --version
COPY deploy/codex-state/prepare.mjs /opt/leo-codex-state-builder.mjs
RUN --network=none node /opt/leo-codex-state-builder.mjs /opt/leo-codex-state
COPY deploy/toolkit /opt/leo-toolkit
RUN chmod +x /opt/leo-toolkit/android.mjs && ln -s /opt/leo-toolkit/android.mjs /usr/local/bin/leo-android
RUN --mount=type=secret,id=github_token,env=GITHUB_TOKEN /usr/local/bin/node /opt/leo-toolkit/manage.mjs install
# Keep the Codex npm launcher on the manager runtime even in older Node projects.
RUN ln -s /usr/local/bin/node /pnpm/bin/node
COPY deploy/toolkit/profile.sh /etc/profile.d/leo-toolkit.sh
ENV LEO_TOOLKIT_DIR=/opt/leo-toolkit
ENV PATH=/usr/local/bin:/home/node/.local/share/mise/shims:/usr/local/share/mise/shims:$PATH
# Pin and verify the server-side 1Password CLI used by scoped workspace tools.
RUN curl -fsSL https://cache.agilebits.com/dist/1P/op2/pkg/v2.39.0/op_linux_amd64_v2.39.0.zip -o /tmp/op.zip \
    && echo '6fba7f376b6c6dec49f41b06408930a43ad064cce103c6a2ce5b3d0413a86434  /tmp/op.zip' | sha256sum -c - \
    && unzip /tmp/op.zip op -d /usr/local/bin && chmod 755 /usr/local/bin/op && rm /tmp/op.zip
COPY --from=backend /usr/local/bin/leo /usr/local/bin/leo
COPY --from=build --chown=node:node /app/dist ./dist
COPY --from=build --chown=node:node /app/package.json ./package.json
RUN install -d -o node -g node /data /workspaces /home/node/.agents /home/node/.agents/skills /home/node/.codex \
    && chown node:node /home/node /app
ARG VCS_REF=development
ENV APP_COMMIT=$VCS_REF APP_RUNTIME_ID=$VCS_REF APP_CODEX_VERSION=$CODEX_VERSION APP_GH_VERSION=$GH_VERSION
USER node
VOLUME ["/data", "/home/node", "/workspaces"]
EXPOSE 4310
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --start-interval=1s CMD /usr/local/bin/node -e "fetch('http://127.0.0.1:4310/health').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"
CMD ["/usr/local/bin/leo", "serve"]

# Guest kernel is built from a pinned upstream LTS source, not demo VM assets.
FROM debian:bookworm-slim AS guest-kernel
RUN apt-get update && apt-get install -y --no-install-recommends build-essential bc bison flex libssl-dev libelf-dev curl ca-certificates xz-utils && rm -rf /var/lib/apt/lists/*
WORKDIR /kernel
RUN curl -fsSL https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.12.109.tar.xz -o linux.tar.xz \
    && echo '5484e552a334e15019f4aeba89e5b58f04651cf2f4e24e04de9f152f1c38e3fa  linux.tar.xz' | sha256sum -c - \
    && tar -xf linux.tar.xz --strip-components=1 && rm linux.tar.xz
COPY deploy/microvm/kernel.config /tmp/leo.config
# Guests use virtio and a serial console; omit unused physical-device families.
RUN make x86_64_defconfig && scripts/kconfig/merge_config.sh -m .config /tmp/leo.config \
    && scripts/config --disable USB --disable DRM --disable FB --disable SOUND --disable MEDIA_SUPPORT \
        --disable WLAN --disable WIRELESS --disable BT --disable HID --disable INPUT \
        --disable SCSI --disable ATA --disable BLK_DEV_MD --disable MMC --disable FIREWIRE \
        --disable MODULES --disable DEBUG_INFO --disable DEBUG_INFO_DWARF_TOOLCHAIN_DEFAULT \
    && make olddefconfig \
    && for option in KVM KVM_INTEL KVM_AMD VMGENID; do grep -qx "CONFIG_${option}=y" .config || exit 1; done \
    && make -j8 vmlinux && strip --strip-debug vmlinux \
    && mv vmlinux /tmp/leo-vmlinux && make clean && mv /tmp/leo-vmlinux vmlinux

FROM runtime AS guest
USER root
COPY --chmod=755 deploy/microvm/install-docker /opt/leo-vm/install-docker
RUN /opt/leo-vm/install-docker
COPY --chmod=755 deploy/microvm/init /sbin/leo-init
COPY --chmod=755 deploy/microvm/docker /usr/local/bin/docker

FROM debian:bookworm-slim AS guest-disk
RUN apt-get update && apt-get install -y --no-install-recommends e2fsprogs zstd && rm -rf /var/lib/apt/lists/*
RUN --mount=from=guest,target=/rootfs,ro truncate -s 8G /root.ext4 && mkfs.ext4 -q -F -d /rootfs /root.ext4 \
    && zstd -T2 -3 /root.ext4 -o /root.ext4.zst && rm /root.ext4

FROM runtime AS final
USER root
RUN apt-get update && apt-get install -y --no-install-recommends iptables e2fsprogs util-linux fuse3 \
    && rm -rf /var/lib/apt/lists/* \
    && curl -fsSL https://github.com/firecracker-microvm/firecracker/releases/download/v1.17.0/firecracker-v1.17.0-x86_64.tgz -o /tmp/firecracker.tgz \
    && echo '06094a1108ae9e82aa4c23a775aa92758f53f1175d422270d9d6162cb9ade558  /tmp/firecracker.tgz' | sha256sum -c - \
    && tar -xzf /tmp/firecracker.tgz -C /tmp \
    && cp /tmp/release-v1.17.0-x86_64/jailer-v1.17.0-x86_64 /usr/local/bin/jailer \
    && rm -rf /tmp/firecracker.tgz /tmp/release-v1.17.0-x86_64
COPY --from=firecracker /usr/local/bin/firecracker /usr/local/bin/firecracker
COPY --from=guest-kernel /kernel/vmlinux /opt/leo-vm/vmlinux
COPY --from=guest-kernel /kernel/.config /opt/leo-vm/kernel.config
COPY --from=guest-kernel /kernel/COPYING /opt/leo-vm/KERNEL-COPYING
COPY --from=guest-kernel /kernel/LICENSES /opt/leo-vm/kernel-licenses
COPY --from=guest-disk /root.ext4.zst /opt/leo-vm/root.ext4.zst
COPY --chmod=755 deploy/microvm/init deploy/microvm/docker deploy/microvm/install-docker /opt/leo-vm/
COPY --chmod=755 deploy/nodes /opt/leo-node
USER node
