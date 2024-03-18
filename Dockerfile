FROM rustlang/rust:nightly-bookworm as builder
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update --yes && \
    apt-get install --yes --no-install-recommends \
    libclang-dev \
    libudev-dev \
    libssl-dev \
    pkg-config \
    gcc \
    cmake \
    git \
    gcc \
    protobuf-compiler \
    clang

RUN rustup target add wasm32-unknown-unknown
WORKDIR /usr/src/app
COPY . .
RUN cargo build --locked --release
RUN cd target/release \
    && chmod +x vault oracle faucet

FROM debian:bookworm as production
ENV DEBIAN_FRONTEND=noninteractive
ENV HOME /usr/src/app

WORKDIR $HOME
RUN apt-get update --yes \
    && apt-get install -y --no-install-recommends openssl ca-certificates

COPY --from=builder ["$HOME/target/release/vault", "$HOME/target/release/oracle", "$HOME/target/release/faucet", "/usr/local/bin/"]

# faucet, vault, oracle are available in PATH
