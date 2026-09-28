#!/usr/bin/env bash
# Build the current ICU code and every supported package.
set -euo pipefail
cd "$(dirname "$0")"
command -v python3 >/dev/null || { echo 'ICU build needs Python 3.11 or newer.' >&2; exit 1; }
python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))' || {
    echo 'ICU build needs Python 3.11 or newer.' >&2; exit 1;
}
exec python3 packaging/build_all.py "$@"
