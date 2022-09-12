ARG BUILDER_DIR=/srv/farcaster


FROM rust:bullseye as builder

ARG SRC_DIR=/usr/local/src/farcaster
ARG BUILDER_DIR

RUN apt-get update -y && apt-get install -y --no-install-recommends apt-utils
RUN DEBIAN_FRONTEND=noninteractive apt-get install -y \
        libssl-dev \
        pkg-config \
        build-essential \
        cmake

WORKDIR "$SRC_DIR"

COPY src ${SRC_DIR}/src
COPY build.rs Cargo.lock Cargo.toml LICENSE README.md ${SRC_DIR}/

WORKDIR ${SRC_DIR}

RUN mkdir "${BUILDER_DIR}"

RUN cargo install --path . --root "${BUILDER_DIR}" --bins --all-features --locked


FROM debian:bullseye-slim

ARG BUILDER_DIR
ARG BIN_DIR=/usr/local/bin
ARG DATA_DIR=/var/lib/farcaster
ARG USER=farcasterd

RUN apt-get update -y \
    && apt-get install -y \
        libzmq3-dev \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/* /tmp/* /var/tmp/*

RUN adduser --home "${DATA_DIR}" --shell /bin/bash --disabled-login \
        --gecos "${USER} user" ${USER}

COPY --from=builder --chown=${USER}:${USER} \
     "${BUILDER_DIR}/bin/" "${BIN_DIR}"

RUN swap-cli completion bash >> .bashrc

WORKDIR "${BIN_DIR}"
USER ${USER}

VOLUME "$DATA_DIR"

EXPOSE 9735

ENTRYPOINT ["farcasterd"]
