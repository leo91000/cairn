#!/usr/bin/env bash
set -euo pipefail

if test -f /etc/docker/daemon.json; then
  sudo jq '.features = (.features // {}) | .features["containerd-snapshotter"] = true' /etc/docker/daemon.json | sudo tee /etc/docker/daemon.json.new > /dev/null
else
  printf '%s\n' '{"features":{"containerd-snapshotter":true}}' | sudo tee /etc/docker/daemon.json.new > /dev/null
fi
sudo mv /etc/docker/daemon.json.new /etc/docker/daemon.json
sudo systemctl restart docker
docker info -f '{{ .DriverStatus }}' | grep -q 'io.containerd.snapshotter.v1'
docker buildx inspect default
