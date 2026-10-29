# The CI gate

Every push and pull request to `main` on GitHub runs `.github/workflows/ci.yml`. It's meant to be
a gate, but it doesn't block anything yet: `main` has no branch protection, so a red run can still
be merged, and it's on whoever merges to check it's green. It checks:

- **Format:** `cargo fmt --all --check`. `rustfmt.toml` sets 200 columns, because the code is
  written wide on purpose.
- **Clippy:** `cargo clippy` with `-D warnings` on the crates that build without the native
  prefixes: cc-proto, cc-host, cc-home, cc-scan, cc-install, cc-pointer and cc-hello. Two lints
  are warnings, not errors, for now, and the job counts them: `undocumented_unsafe_blocks` and
  `missing_safety_doc`. There are 163 unsafe blocks in those crates without a `// SAFETY:` comment
  yet. A new one should come with its comment.
- **Tests:** `cargo test` for the same crates on the musl target, on x86_64 and on aarch64. The
  tests get a throwaway `HOME` and their own `CC_PANELS_SOCKET`, so they never touch a real
  config or a running Desktop.
- **cc-panels:** built and tested in the Fedora 44 image, with FreeRDP and libvncclient built
  the way `cc-home install` builds them. It doesn't block a merge yet, because it's new and
  slow. It becomes part of the gate once it's reliably green.
- **cargo-deny:** RustSec advisories, the licenses of every dependency (the allowed list is in
  `deny.toml`), and where crates come from. Two versions of the same crate only warn.

The workspace also sets lints for every crate (`[workspace.lints]` in `Cargo.toml`):
`unsafe_op_in_unsafe_fn` is an error, and cc-proto forbids `unsafe` altogether. The minimum Rust
is 1.89 (`rust-version`), because of `File::lock`.

## Running it locally

In the dev container (`./cc-box bash`, or `distrobox enter control-center`), with the cc-rust
toolchain:

```sh
export RUSTUP_HOME=$HOME/.local/share/cc-rust/rustup CARGO_HOME=$HOME/.local/share/cc-rust/cargo
export PATH=$CARGO_HOME/bin:$PATH
rustup component add rustfmt clippy        # once
C="-p cc-proto -p cc-host -p cc-home -p cc-scan -p cc-install -p cc-pointer -p cc-hello"

cargo fmt --all --check                    # or cargo fmt --all to fix it
cargo clippy $C --all-targets -- -D warnings -A clippy::undocumented_unsafe_blocks -A clippy::missing_safety_doc

T=aarch64-unknown-linux-musl
cargo build --release --target $T -p cc-host
mkdir -p /tmp/cc-ci-home
HOME=/tmp/cc-ci-home RUSTUP_HOME=$RUSTUP_HOME CARGO_HOME=$CARGO_HOME CC_PANELS_SOCKET=cc-ci-test \
  CC_HOST_BIN=$PWD/target/$T/release/cc-host \
  cargo test --target $T -p cc-proto -p cc-host -p cc-home -p cc-scan -p cc-install -p cc-pointer
```

cargo-deny is `cargo install --locked cargo-deny`, then `cargo deny check`.

cc-panels' tests need the FreeRDP and VNC prefixes. A worktree doesn't have them, so point at the
main checkout's: `FREERDP_PREFIX=…/panels/third_party/prefix VNC_PREFIX=…/panels/third_party/vnc-prefix`,
with that prefix's `lib64` and `/opt/steamvr/bin/linuxarm64` on `LD_LIBRARY_PATH`.

Our Forgejo copy (`forge`) doesn't run Actions; the gate runs on GitHub.
