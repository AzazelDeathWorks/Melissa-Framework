ARG BASE=melissa-frontend-base:d853da333281
FROM ${BASE}
USER root
COPY package.json package-lock.json /opt/melissa-frontend/
RUN cd /opt/melissa-frontend && /opt/node/bin/npm ci --ignore-scripts --no-audit --no-fund && /opt/node/bin/node -e "console.log(require('./node_modules/axe-core/package.json').version)" && /opt/node/bin/node node_modules/chrome-devtools-mcp/build/src/bin/chrome-devtools-mcp.js --version
