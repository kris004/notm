# notm GitHub Actions runner on private-ci-host

This Compose project runs the repository-scoped `notm-private-runner` runner in a
read-only Ubuntu 24.04 container. The image contains the native GTK/WebKitGTK,
Weston, Xvfb, and pinned Rust toolchain used by CI.

The container deliberately has no Docker socket, host home, devices, or other
host filesystem mounts. It drops every capability and runs as UID 1000 with
bounded CPU, memory, PIDs, and shared memory. Its only writable persistent
mount is the dedicated runner home at
`~/.local/share/notm-actions-runner/home`.

Private CI host requires host networking because its firewall blocks Docker bridge DNS
and egress. The runner publishes no ports, but workflow code can reach private-ci-host's
host network and LAN. Keep this runner restricted to trusted private-repository
workflows and remove it before making the repository public.

## Operations

Run these commands from the deployed copy of this directory:

```sh
docker compose build
docker compose up -d
docker compose ps
docker compose logs --tail=100 runner
```

On first startup, the entrypoint copies the runner distribution into the
dedicated home and waits. Register it without storing the one-hour token in the
Compose environment:

```sh
docker compose exec -T -w /home/ubuntu/actions-runner runner \
  ./config.sh --unattended \
  --url https://github.com/kris004/notm \
  --token "$REGISTRATION_TOKEN" \
  --name notm-private-runner \
  --labels notm-private-runner \
  --work _work
docker compose restart runner
```

The GitHub runner updates itself in the writable home. Rebuild the image when
updating the pinned Ubuntu base, Rust toolchain, or native CI dependencies.

Before making `notm` public, change the workflow back to a GitHub-hosted label,
stop this container, and remove the repository runner registration. After the
registration is removed, the local container and its state can be removed with:

```sh
docker compose down --rmi local
rm -rf ~/.local/share/notm-actions-runner
```
