# Xanadu Vibe Kanban Deployment

Gavin's fork can deploy Vibe Kanban to `xanadu-host` with the same GitHub Actions shape used by Open WebUI.

- workflow: `.github/workflows/deploy-xanadu-vibe-kanban.yml`
- deploy script: `scripts/deploy-xanadu-vibe-kanban.sh`
- branch: `main`
- environment: `xanadu-production`
- public URL: `https://vibe.yxanadu.com`
- launchd service: `com.xanadu.vibe-kanban`
- loopback app: `http://127.0.0.1:3063`
- loopback preview proxy: `http://127.0.0.1:3064`

The workflow expects a self-hosted runner available to `gavinanelson/vibe-kanban` with these labels:

```text
self-hosted
macOS
ARM64
xanadu-host
vibe-kanban-deploy
```

Open WebUI currently has an online repo runner named `xanadu-host-open-webui-deploy`. Vibe Kanban needs an equivalent runner registration, or that runner must be moved to a scope where this fork can use it.

Manual deploy from a configured `xanadu-host` checkout:

```bash
GITHUB_REF_NAME=main ./scripts/deploy-xanadu-vibe-kanban.sh
```

Manual verification on `xanadu-host`:

```bash
curl -s http://127.0.0.1:3063/health
curl -ks https://vibe.yxanadu.com/health
launchctl print gui/$(id -u)/com.xanadu.vibe-kanban
gh run list -R gavinanelson/vibe-kanban --workflow deploy-xanadu-vibe-kanban.yml --limit 5
```
