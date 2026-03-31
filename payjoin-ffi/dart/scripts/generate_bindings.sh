#!/usr/bin/env bash
set -euo pipefail

OS=$(uname -s)
echo "Running on $OS"

dart --version
dart pub get

# Install Rust targets if on macOS
if [[ $OS == "Darwin" ]]; then
    LIBNAME=libpayjoin_ffi.dylib
elif [[ $OS == "Linux" ]]; then
    LIBNAME=libpayjoin_ffi.so
else
    echo "Unsupported os: $OS"
    exit 1
fi

cd ../
echo "Generating payjoin dart..."
cargo build --features dart --profile dev
cargo run --features dart --profile dev --bin uniffi-bindgen -- --library ../target/debug/$LIBNAME --language dart --out-dir dart/lib/

if [[ $OS == "Darwin" ]]; then
    TEST_UTILS_LIBNAME=libpayjoin_ffi_test_utils.dylib
else
    TEST_UTILS_LIBNAME=libpayjoin_ffi_test_utils.so
fi

echo "Generating payjoin test utils dart..."
cargo build -p payjoin-ffi-test-utils --features dart --profile dev
cargo run -p payjoin-ffi-test-utils --features dart --profile dev --bin uniffi-bindgen-test-utils -- --library ../target/debug/$TEST_UTILS_LIBNAME --language dart --out-dir dart/lib/

echo "All done!"
