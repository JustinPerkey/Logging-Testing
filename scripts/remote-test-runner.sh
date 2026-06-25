#!/usr/bin/env bash
#
# Cargo target runner that lets you BUILD logbench's tests/benches/binary on one
# machine and RUN them on another ("the device under test").
#
# Cargo invokes a configured runner as:
#
#     remote-test-runner.sh <path-to-compiled-binary> [args...]
#
# This script copies that freshly built binary to a remote device over SSH, runs
# it there (forwarding all arguments), and propagates its exit code back — so a
# cross-compiled `cargo test` behaves exactly as if it ran locally, with the test
# logic (file I/O, the temp dirs it creates, the byte-count assertions) all
# executing on the target device.
#
# It is wired up in `.cargo/config.toml`. When the LOGBENCH_REMOTE env var is
# unset it is a transparent no-op: the binary is executed locally. So having the
# runner configured never gets in the way of an ordinary same-machine build/run —
# you only opt into remote execution by pointing LOGBENCH_REMOTE at a device.
#
# Configuration (all via environment variables):
#
#   LOGBENCH_REMOTE      user@host of the target device. If empty/unset, the
#                        binary runs locally and every var below is ignored.
#   LOGBENCH_REMOTE_DIR  directory on the target to stage binaries in.
#                        Default: /tmp/logbench-tests
#   LOGBENCH_SSH         ssh command to use. Default: "ssh". Override to pass a
#                        port/identity, e.g. LOGBENCH_SSH="ssh -p 2222 -i ~/k"
#   LOGBENCH_SCP         scp command to use. Default: "scp". Override the same
#                        way, e.g. LOGBENCH_SCP="scp -P 2222 -i ~/k"
#   LOGBENCH_KEEP_REMOTE if set to 1, leave the copied binary on the target
#                        instead of deleting it after the run.
#
# Examples:
#
#   # Cross-compile the tests for a Raspberry Pi and run them on it:
#   LOGBENCH_REMOTE=pi@raspberrypi.local \
#     cargo test --release --target aarch64-unknown-linux-gnu
#
#   # Run the benchmark binary itself on the device:
#   LOGBENCH_REMOTE=pi@raspberrypi.local \
#     cargo run --release --target aarch64-unknown-linux-gnu -- --producers 4
#
set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "remote-test-runner: expected a binary path as the first argument" >&2
    exit 2
fi

binary="$1"
shift

# No target device configured -> behave as a plain local executor. This keeps a
# globally-configured runner harmless for ordinary same-machine workflows.
if [[ -z "${LOGBENCH_REMOTE:-}" ]]; then
    exec "$binary" "$@"
fi

remote="$LOGBENCH_REMOTE"
remote_dir="${LOGBENCH_REMOTE_DIR:-/tmp/logbench-tests}"
ssh_cmd="${LOGBENCH_SSH:-ssh}"
scp_cmd="${LOGBENCH_SCP:-scp}"

# A unique remote name so concurrent / repeated runs never clobber each other.
remote_name="$(basename "$binary").$$.$RANDOM"
remote_path="$remote_dir/$remote_name"

# shellcheck disable=SC2086  # ssh/scp commands are intentionally word-split.
cleanup() {
    if [[ "${LOGBENCH_KEEP_REMOTE:-0}" != "1" ]]; then
        $ssh_cmd "$remote" "rm -f '$remote_path'" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT

# shellcheck disable=SC2086
$ssh_cmd "$remote" "mkdir -p '$remote_dir'"
# shellcheck disable=SC2086
$scp_cmd -q "$binary" "$remote:$remote_path"
# shellcheck disable=SC2086
$ssh_cmd "$remote" "chmod +x '$remote_path'"

# Run on the device, forwarding all test/binary arguments, and propagate the
# exit code so `cargo test`/CI sees the real pass/fail result.
status=0
# shellcheck disable=SC2086
$ssh_cmd "$remote" "'$remote_path' $*" || status=$?
exit "$status"
