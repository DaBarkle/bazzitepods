#!/usr/bin/env bash
# Old name of the Kase installer, kept so existing links and commands keep working.
# Everything is done by install.sh (same options); see that file.
set -euo pipefail

# Run as a file: use the installer beside it.
if [[ -f "${BASH_SOURCE[0]:-}" && -f "$(dirname -- "${BASH_SOURCE[0]}")/install.sh" ]]; then
    exec bash "$(dirname -- "${BASH_SOURCE[0]}")/install.sh" "$@"
fi
# Run through `curl ... | bash`: fetch the current installer.
exec bash -c "$(curl -fsSL https://raw.githubusercontent.com/DaBarkle/kase/main/linux-rust/packaging/install.sh)" install.sh "$@"
