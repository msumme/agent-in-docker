# Single-stage: bridge runs on host, container just needs Claude Code
FROM node:22-alpine

# Install packages + glibc compat layer (needed for pre-built Go binaries like bd)
RUN apk add --no-cache bash curl python3 git gcompat

# Install Claude Code CLI
RUN npm install -g @anthropic-ai/claude-code

# Install beads (bd) - pre-built binary
RUN ARCH=$(uname -m | sed 's/aarch64/arm64/;s/x86_64/amd64/') && \
    VERSION=$(curl -fsSL "https://api.github.com/repos/steveyegge/beads/releases/latest" | grep tag_name | cut -d'"' -f4) && \
    curl -fsSL "https://github.com/steveyegge/beads/releases/download/${VERSION}/beads_${VERSION#v}_linux_${ARCH}.tar.gz" \
    | tar -xz -C /usr/local/bin bd 2>/dev/null || \
    echo "Warning: beads binary not available for this architecture"

# Install dolt (for beads backend)
RUN ARCH=$(uname -m | sed 's/aarch64/arm64/;s/x86_64/amd64/') && \
    curl -fsSL "https://github.com/dolthub/dolt/releases/latest/download/dolt-linux-${ARCH}.tar.gz" \
    | tar -xz --strip-components=1 -C /usr/local dolt-linux-${ARCH}/bin/dolt 2>/dev/null || \
    echo "Warning: dolt binary not available for this architecture"

# Workspace
RUN mkdir -p /workspace && chown node:node /workspace

# Copy entrypoint
COPY scripts/entrypoint.sh /opt/entrypoint.sh
RUN chmod +x /opt/entrypoint.sh

WORKDIR /workspace

ENTRYPOINT ["/opt/entrypoint.sh"]
