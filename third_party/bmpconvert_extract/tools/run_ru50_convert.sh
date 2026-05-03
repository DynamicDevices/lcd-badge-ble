#!/usr/bin/env bash
# Run RU50 image conversion with a local venv (etcpak + Pillow).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"
VENV="${RU50_VENV:-$HERE/ru50-venv}"
if [[ ! -x "$VENV/bin/python" ]]; then
  PY="${RU50_PYTHON:-python3}"
  "$PY" -m venv "$VENV"
  "$VENV/bin/pip" install -q -r requirements-ru50.txt
fi
exec "$VENV/bin/python" ru50_convert.py "$@"
