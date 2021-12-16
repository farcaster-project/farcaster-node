FROM ghcr.io/farcaster-project/farcaster-node/farcasterd:latest

ARG BIN_DIR=/usr/local/bin
ARG DATA_DIR=/var/lib/farcaster
ARG USER=farcasterd

COPY --chown=${USER}:${USER} farcasterd.toml ${DATA_DIR}/farcasterd.toml

WORKDIR "${BIN_DIR}"
USER ${USER}

VOLUME "$DATA_DIR"

EXPOSE 9735 9981

ENTRYPOINT ["farcasterd"]

CMD ["--data-dir", "/var/lib/farcaster", "-x", "lnpz://0.0.0.0:9981/?api=esb", "--help"]
