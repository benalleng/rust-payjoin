#!/usr/bin/env bash
set -euo pipefail

OS=$(uname -s)
echo "Running on $OS"

if [[ $OS == "Darwin" ]]; then
    LIBNAME=libpayjoin_ffi.dylib
elif [[ $OS == "Linux" ]]; then
    LIBNAME=libpayjoin_ffi.so
elif [[ $OS == MINGW* || $OS == MSYS* || $OS == CYGWIN* ]]; then
    # Git Bash / MSYS-style shells on Windows
    LIBNAME=payjoin_ffi.dll
else
    echo "Unsupported os: $OS"
    exit 1
fi

# Get the directory where this script is located
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Navigate to payjoin-ffi directory (parent of csharp, which is parent of scripts)
cd "$SCRIPT_DIR/../.."

echo "Generating payjoin C#..."
PAYJOIN_FFI_FEATURES=${PAYJOIN_FFI_FEATURES:-}
GENERATOR_FEATURES="csharp"
if [[ -n $PAYJOIN_FFI_FEATURES ]]; then
    GENERATOR_FEATURES="$GENERATOR_FEATURES,$PAYJOIN_FFI_FEATURES"
fi

cargo build --features "$GENERATOR_FEATURES" --profile dev -j2

# Clean output directory to prevent duplicate definitions
echo "Cleaning csharp/src/ directory..."
mkdir -p csharp/src
rm -f csharp/src/*.cs

# Use the Cargo-managed C# generator pinned in payjoin-ffi/Cargo.toml.
UNIFFI_BINDGEN_LANGUAGE=csharp cargo run --features "$GENERATOR_FEATURES" --profile dev --bin uniffi-bindgen -- \
    --library ../target/debug/$LIBNAME \
    --out-dir csharp/src/

# Copy native library to csharp/lib/ directory for testing
echo "Copying native library..."
mkdir -p csharp/lib
cp ../target/debug/$LIBNAME csharp/lib/$LIBNAME

# Generate test utils bindings from payjoin-ffi-test-utils crate
if [[ $OS == "Darwin" ]]; then
    TEST_UTILS_LIBNAME=libpayjoin_ffi_test_utils.dylib
elif [[ $OS == "Linux" ]]; then
    TEST_UTILS_LIBNAME=libpayjoin_ffi_test_utils.so
else
    TEST_UTILS_LIBNAME=payjoin_ffi_test_utils.dll
fi

echo "Generating payjoin test utils C#..."
cargo build -p payjoin-ffi-test-utils --features csharp --profile dev -j2

UNIFFI_BINDGEN_LANGUAGE=csharp cargo run -p payjoin-ffi-test-utils --features csharp --profile dev --bin uniffi-bindgen-test-utils -- \
    --library ../target/debug/$TEST_UTILS_LIBNAME \
    --out-dir csharp/src/

echo "Copying test utils native library..."
cp ../target/debug/$TEST_UTILS_LIBNAME csharp/lib/$TEST_UTILS_LIBNAME

echo "All done!"
