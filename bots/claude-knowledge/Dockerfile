FROM oven/bun:1-debian AS gizmo-build
WORKDIR /app
COPY bun/package.json bun/bun.lock ./
RUN bun install
COPY bun/ .
RUN bun link

FROM node:22-slim

# Install bun (copy binary to shared path so non-root users can use it)
RUN apt-get update && apt-get install -y curl unzip git && \
    curl -fsSL https://bun.sh/install | bash && \
    cp /root/.bun/bin/bun /usr/local/bin/bun && \
    chmod 755 /usr/local/bin/bun

# Install Claude Code
RUN npm install -g @anthropic-ai/claude-code

# Copy gizmo
COPY --from=gizmo-build /app /opt/gizmo
COPY --from=gizmo-build /root/.bun/install/global/node_modules /root/.bun/install/global/node_modules
RUN printf '#!/bin/sh\ncd /opt/gizmo && bun run cli.ts "$@"\n' > /usr/local/bin/gizmo && chmod +x /usr/local/bin/gizmo

# Create unprivileged user for running claude
RUN useradd -m -s /bin/sh agent && \
    chmod -R 755 /opt/gizmo

COPY bots/claude-knowledge/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

COPY bots/claude-knowledge/prompt.md /prompt.md

VOLUME /knowledge

ENTRYPOINT ["/entrypoint.sh"]
