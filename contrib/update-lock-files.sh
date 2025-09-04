#!/usr/bin/env bash
#
# Update the minimal/recent lock file

set -euo pipefail

NIGHTLY=$(cat nightly-version)

rm -f Cargo.lock && cargo +"$NIGHTLY" check --all-features -Z direct-minimal-versions

# rm -f Cargo.lock && cargo +"$NIGHTLY" check --all-features -Z minimal-versions
cp -f Cargo.lock Cargo-minimal.lock

cp -f Cargo-recent.lock Cargo.lock
cargo check --all-features
cp -f Cargo.lock Cargo-recent.lock
