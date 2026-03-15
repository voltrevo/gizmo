FROM oven/bun:1-debian AS gizmo-build
WORKDIR /app
COPY bun/package.json bun/bun.lock ./
RUN bun install
COPY bun/ .
RUN bun link

FROM node:22-slim

# Install bun (copy binary to shared path so non-root users can use it)
RUN apt-get update && apt-get install -y curl unzip git jq && \
    curl -fsSL https://bun.sh/install | bash && \
    cp /root/.bun/bin/bun /usr/local/bin/bun && \
    chmod 755 /usr/local/bin/bun

# Install Claude Code
RUN npm install -g @anthropic-ai/claude-code

# Copy gizmo
COPY --from=gizmo-build /app /opt/gizmo
COPY --from=gizmo-build /root/.bun/install/global/node_modules /root/.bun/install/global/node_modules
RUN printf '#!/bin/sh\ncd /opt/gizmo && bun run cli.ts "$@"\n' > /usr/local/bin/gizmo && chmod +x /usr/local/bin/gizmo

RUN chmod -R 755 /opt/gizmo

COPY bots/claude-knowledge/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# Multi-agent components
COPY bots/claude-knowledge/coordinator.ts /coordinator.ts
COPY bots/claude-knowledge/prompt-router.md /prompt-router.md
COPY bots/claude-knowledge/prompt-worker.md /prompt-worker.md


ENTRYPOINT ["/entrypoint.sh"]
