# memnoth MCP server (StreamableHTTP transport), packaged from an already built binary.
#
# The binary is the root `memnoth` executable (`memnoth mcp serve --http`), built natively for
# each platform by the CI job. The context holds one file per architecture and buildx's automatic
# platform arg picks it — a default on that arg is how an arm64 image once shipped an amd64
# binary that only failed at run time.
#
# The server drops capabilities through compose (`cap_drop: ALL`, `read_only: true`), so it runs
# as root and init.sh prepares the volume before exec.

FROM debian:bookworm-slim

RUN apt-get update -qq \
  && apt-get install -y -qq --no-install-recommends ca-certificates curl \
  && rm -rf /var/lib/apt/lists/*

WORKDIR /app

ARG TARGETARCH
RUN test -n "${TARGETARCH}" || (echo "TARGETARCH is unset — build with buildx --platform" && exit 1)
COPY memnoth-${TARGETARCH} /usr/local/bin/memnoth
COPY init.sh /usr/local/bin/memnoth-init.sh
RUN chmod 0755 /usr/local/bin/memnoth /usr/local/bin/memnoth-init.sh

# Persistent vault/coord store (MEMNOTH_HOME -> /data/.memnoth/vault.db).
VOLUME ["/data"]

ENV MEMNOTH_HOME=/data
ENV MEMNOTH_MCP_HTTP_BIND=0.0.0.0:7842
ENV MEMNOTH_MCP_PROFILE=full
# MEMNOTH_MCP_HTTP_TOKEN is injected from the VM .env at compose time (never baked in).

EXPOSE 7842

USER root

STOPSIGNAL SIGTERM

# Readiness: one JSON-RPC initialize against the port. A 401/400 with the token missing still
# proves the listener is up, so the check is `curl` succeeding at the TCP/HTTP layer.
HEALTHCHECK --interval=30s --timeout=5s --retries=3 --start-period=20s \
  CMD curl -fsS -o /dev/null -X POST http://127.0.0.1:7842/mcp \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer ${MEMNOTH_MCP_HTTP_TOKEN}" \
    -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"hc","version":"0"}}}' \
    || exit 1

ENTRYPOINT ["/usr/local/bin/memnoth-init.sh"]
CMD ["mcp", "serve", "--http", "--db", "/data/.memnoth/vault.db"]
