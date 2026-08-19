#!/usr/bin/bash

set -euo pipefail

readonly runner_dir=/home/ubuntu/actions-runner
readonly runner_dist=/opt/actions-runner-dist

if [[ ! -x "$runner_dir/run.sh" ]]; then
  install -d -m 0755 "$runner_dir"
  cp -a "$runner_dist/." "$runner_dir/"
fi

if [[ ! -f "$runner_dir/.runner" ]]; then
  printf '%s\n' \
    'notm runner files are installed but not registered; waiting for config.sh'
  exec sleep infinity
fi

cd "$runner_dir"
exec ./run.sh
