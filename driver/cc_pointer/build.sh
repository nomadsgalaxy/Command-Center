#!/usr/bin/env bash
# Builds the cc_pointer SteamVR driver (crates/cc-pointer, Rust) into build/driver_cc_pointer.so
# in the control-center container and runs its selftest. vrserver loads the .so on the SteamOS
# host, not in the container, so it can only use glibc symbols the host has (2.39, the
# container's is newer), and it can't need libm or libstdc++. It does need libgcc_s.so.1 (Rust's
# unwinder), which SteamOS has in /usr/lib.
# The full fake-runtime harness takes about 21 s, see crates/cc-pointer/tests/harness.rs.
# Usage: driver/cc_pointer/build.sh
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
exec "$here/../../cc-box" bash -c 'set -euo pipefail
cd "$1"
# Skip the workspace rustflags: their rpaths are for our programs, not a library vrserver loads.
# It gets its own target dir so that doesn'"'"'t rebuild the workspace'"'"'s. No debug info (it'"'"'s 10 MB).
export RUSTFLAGS= CARGO_PROFILE_RELEASE_STRIP=debuginfo
c="--quiet --manifest-path ../../Cargo.toml --release -p cc-pointer --target-dir build/target"
cargo test $c --lib
cargo build $c
cp build/target/release/libdriver_cc_pointer.so build/driver_cc_pointer.so
max=$(objdump -T build/driver_cc_pointer.so | grep -oE "GLIBC_[0-9.]+" | sort -uV | tail -1)
echo "built build/driver_cc_pointer.so, newest glibc symbol: $max"
[ "$(printf "%s\n" "$max" GLIBC_2.39 | sort -V | tail -1)" = GLIBC_2.39 ] || { echo "needs newer glibc than the host has" >&2; exit 1; }
! objdump -p build/driver_cc_pointer.so | grep -E "NEEDED.*lib(m|stdc\+\+)\." || { echo "links libm or libstdc++" >&2; exit 1; }
nm -D --defined-only build/driver_cc_pointer.so | grep -q " T HmdDriverFactory$" || { echo "HmdDriverFactory not exported" >&2; exit 1; }' _ "$here"
