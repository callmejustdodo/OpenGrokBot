#!/bin/sh
# Regenerates apps/desktop/src/ipc/generated from formation-proto: one file per
# type from ts-rs, plus commands.ts (method -> params and result). Clearing the
# folder first drops files for types that no longer exist.
# Usage: pnpm -C apps/desktop ipc:generate   (CI runs it and checks for a diff)
set -eu
cd "$(dirname "$0")/../../.."
rm -rf apps/desktop/src/ipc/generated
cargo test -q -p formation-proto export_bindings
cargo test -q -p formation-desktop --test typescript_commands
