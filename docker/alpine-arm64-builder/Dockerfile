FROM rust:1.92-alpine3.22

RUN apk add --no-cache \
    build-base \
    fontconfig-dev \
    freetype-dev \
    git \
    libdrm-dev \
    libinput-dev \
    libxkbcommon-dev \
    linux-headers \
    mesa-dev \
    pkgconf \
    wayland-dev

WORKDIR /workspace

ENV CARGO_TERM_COLOR=always

CMD ["cargo", "build", "--workspace", "--release"]