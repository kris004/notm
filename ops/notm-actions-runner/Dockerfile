FROM ubuntu@sha256:561618e2c15bf2397621dd04f96926663a3b5616c189cf7e38db7e82f5c538ea

ARG RUNNER_VERSION=2.336.0
ARG RUNNER_SHA256=04cf0be1aff4c3ec3554466c39124ca250e3effd8873bb7e8d68535aa9505d5d
ARG RUSTUP_VERSION=1.29.0
ARG RUSTUP_SHA256=4acc9acc76d5079515b46346a485974457b5a79893cfb01112423c89aeb5aa10
ARG RUST_TOOLCHAIN=1.97.1

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        build-essential \
        ca-certificates \
        clang \
        curl \
        dbus-daemon \
        fontconfig \
        fonts-dejavu-core \
        git \
        gzip \
        jq \
        libclang-dev \
        libgtk-4-dev \
        libgtksourceview-5-dev \
        libicu74 \
        libkrb5-3 \
        liblttng-ust1t64 \
        libnotmuch-dev \
        libssl3t64 \
        libstdc++6 \
        libwebkitgtk-6.0-dev \
        pkg-config \
        procps \
        tar \
        unzip \
        weston \
        xauth \
        xvfb \
        xz-utils \
        zip \
        zlib1g \
        zstd \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL \
        -o /tmp/actions-runner.tar.gz \
        "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-linux-x64-${RUNNER_VERSION}.tar.gz" \
    && printf '%s  %s\n' "$RUNNER_SHA256" /tmp/actions-runner.tar.gz \
        | sha256sum --check --strict \
    && mkdir -p /opt/actions-runner-dist \
    && tar -xzf /tmp/actions-runner.tar.gz -C /opt/actions-runner-dist \
    && chown -R 1000:1000 /opt/actions-runner-dist \
    && rm /tmp/actions-runner.tar.gz

ENV RUSTUP_HOME=/opt/rustup
ENV CARGO_HOME=/opt/cargo

RUN curl -fsSL \
        -o /tmp/rustup-init \
        "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/x86_64-unknown-linux-gnu/rustup-init" \
    && printf '%s  %s\n' "$RUSTUP_SHA256" /tmp/rustup-init \
        | sha256sum --check --strict \
    && chmod 0755 /tmp/rustup-init \
    && /tmp/rustup-init -y --no-modify-path --profile minimal \
        --default-toolchain "$RUST_TOOLCHAIN" \
    && /opt/cargo/bin/rustup component add \
        --toolchain "$RUST_TOOLCHAIN" clippy rustfmt \
    && rm /tmp/rustup-init \
    && chmod -R a+rX /opt/rustup /opt/cargo

COPY --chmod=0755 entrypoint.sh /usr/local/bin/notm-runner-entrypoint

ENV CARGO_HOME=/home/ubuntu/.cargo
ENV HOME=/home/ubuntu
ENV LANG=C.UTF-8
ENV PATH=/opt/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

USER 1000:1000
WORKDIR /home/ubuntu
ENTRYPOINT ["/usr/local/bin/notm-runner-entrypoint"]
