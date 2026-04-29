# Xanadu Vibe Kanban Deployment

Gavin's fork can deploy Vibe Kanban to `xanadu-host` with the same GitHub Actions shape used by Open WebUI.

- workflow: `.github/workflows/deploy-xanadu-vibe-kanban.yml`
- deploy script: `scripts/deploy-xanadu-vibe-kanban.sh`
- branch: `main`
- environment: `xanadu-production`
- public URL: `https://vibe.yxanadu.com`
- service state: `/home/coder/.local/state/paddys/vibe-kanban-deploy`
- loopback app: `http://127.0.0.1:8080`
- loopback backend API: `http://127.0.0.1:8082`
- loopback preview proxy: `http://127.0.0.1:8081`

The workflow expects a self-hosted runner available to `gavinanelson/vibe-kanban` with these labels:

```text
self-hosted
Linux
ARM64
vibe-kanban-deploy
```

Open WebUI uses a separate repo-scoped runner. Vibe Kanban needs its own runner registered to `gavinanelson/vibe-kanban` with the labels above.

Manual deploy from a configured `xanadu-host` checkout:

```bash
GITHUB_REF_NAME=main ./scripts/deploy-xanadu-vibe-kanban.sh
```

Manual verification on `xanadu-host`:

```bash
curl -s http://127.0.0.1:8080/
curl -s http://127.0.0.1:8080/api/info
curl -s http://127.0.0.1:8082/api/info
curl -ks https://vibe.yxanadu.com/api/info
cat /home/coder/.local/state/paddys/vibe-kanban-deploy/vibe-kanban.pid
gh run list -R gavinanelson/vibe-kanban --workflow deploy-xanadu-vibe-kanban.yml --limit 5
```
