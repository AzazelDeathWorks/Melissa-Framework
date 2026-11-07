# Owned frontend SDK

Extend the exact services SDK already reviewed for the project. `prepare.ps1`
copies pinned inputs into a new directory; `-Build` checks the immutable base,
preserves existing aliases, and builds an image without pulling another base.
The npm lock pins every transitive package and integrity digest. Dependency
downloads occur only in this explicit SDK preparation, not model workers.

```powershell
./scripts/toolchains/frontend/prepare.ps1 -OutputDirectory C:/private/frontend-sdk-01 -Build
```

Use the resulting `image.id` in the reviewed project environment. It adds
axe-core 4.14.0, pixelmatch 8.0.0, pngjs 7.0.0 and Chrome DevTools MCP 1.12.0.
Playwright 1.64.0/Chromium and PostgreSQL are inherited. A missing capability is
BLOCKED by preflight; installing packages during a task is not a fallback.

DevTools is optional, confined to the same task's Chromium, with usage telemetry,
CrUX URL submission, caller JS evaluation, source maps and file navigation off.
It uses no personal browser/profile, host CDP endpoint, WAN or Docker socket.

Primary implementation references:
- [Playwright network](https://playwright.dev/docs/network)
- [Playwright service workers](https://playwright.dev/docs/service-workers)
- [axe-core API](https://github.com/dequelabs/axe-core/blob/develop/doc/API.md)
- [Chrome DevTools MCP configuration](https://github.com/ChromeDevTools/chrome-devtools-mcp/blob/main/docs/configuration.md)
- [Chrome DevTools MCP tools](https://github.com/ChromeDevTools/chrome-devtools-mcp/blob/main/docs/tool-reference.md)
