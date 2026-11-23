#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
FIXTURE="$ROOT_DIR/Tests/Fixtures/QUICPeer"
"${CARGO:-cargo}" build --locked --manifest-path "$FIXTURE/Cargo.toml" --target-dir "$FIXTURE/target"
python3 - "$ROOT_DIR" "$@" <<'PY'
import os
import selectors
import subprocess
import sys

root = sys.argv[1]
peer = subprocess.Popen(
    [root + '/Tests/Fixtures/QUICPeer/target/debug/winbridge-quic-test-peer'],
    stdout=subprocess.PIPE, text=True)
try:
    with selectors.DefaultSelector() as ready:
        ready.register(peer.stdout, selectors.EVENT_READ)
        if not ready.select(timeout=10):
            raise RuntimeError('Loopback Quinn fixture did not start')
    line = peer.stdout.readline().strip()
    if not line.startswith('PORT='):
        raise RuntimeError('Unexpected Quinn fixture startup: ' + line)
    env = os.environ.copy()
    env['WINBRIDGE_QUIC_TEST_PORT'] = line.split('=', 1)[1]
    result = subprocess.run([root + '/scripts/run_tests.sh', *sys.argv[2:]],
                            env=env, timeout=240)
    sys.exit(result.returncode)
finally:
    peer.terminate()
    try:
        output, _ = peer.communicate(timeout=5)
    except subprocess.TimeoutExpired:
        peer.kill()
        output, _ = peer.communicate()
    print(output, end='')
PY
