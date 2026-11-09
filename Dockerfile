FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY migrations ./migrations
RUN cargo build --release -p tasklane-server && strip target/release/tasklane

FROM node:22-bookworm-slim
# git: workspaces and pushes. openssl: the GitHub plug-in signs its app token with it.
# python3: the shipped plug-ins. ca-certificates: HTTPS to the ticket source and the model API.
# node + the Claude Code CLI: what the shipped agent-engine plug-in runs.
ARG CLAUDE_CODE_VERSION=latest
# the Docker client only (used by the sandbox option); the Docker socket is mounted only if you choose that option
ARG DOCKER_CLI_VERSION=27.3.1
RUN apt-get update \
 && apt-get install -y --no-install-recommends git openssl python3 ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && npm install -g @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION} \
 && ARCH=$(uname -m) \
 && curl -fsSL "https://download.docker.com/linux/static/stable/${ARCH}/docker-${DOCKER_CLI_VERSION}.tgz" | tar xz --strip-components=1 -C /usr/local/bin docker/docker \
 && useradd --system --create-home --uid 10001 tasklane \
 && mkdir -p /data && chown tasklane /data
COPY --from=build /src/target/release/tasklane /usr/local/bin/tasklane
COPY plugins /opt/tasklane/plugins
USER tasklane
ENV TASKLANE_DATA_DIR=/data TASKLANE_BIND=0.0.0.0:8080 \
    TASKLANE_SOURCE_CMD="python3 -I /opt/tasklane/plugins/source-github/source_github.py" \
    TASKLANE_ENGINE_CMD="python3 -I /opt/tasklane/plugins/engine-claude/engine_claude.py"
VOLUME /data
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --retries=3 CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1
ENTRYPOINT ["tasklane"]
CMD ["serve"]
