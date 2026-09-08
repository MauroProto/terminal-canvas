#!/usr/bin/env bash
# Source from build scripts; the caller owns the process-scoped environment.
tc_select_rust_toolchain() {
  local repo_root="$1" rustc_release cargo_version
  TC_RUST_TOOLCHAIN="$(awk -F'"' '/^[[:space:]]*channel[[:space:]]*=/ {print $2; exit}' "$repo_root/rust-toolchain.toml")"
  if [[ ! "$TC_RUST_TOOLCHAIN" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo 'rust-toolchain.toml must pin an exact stable Rust version' >&2
    return 1
  fi
  TC_CARGO="$(rustup which --toolchain "$TC_RUST_TOOLCHAIN" cargo)" || return 1
  TC_RUSTC="$(rustup which --toolchain "$TC_RUST_TOOLCHAIN" rustc)" || return 1
  if [[ ! -x "$TC_CARGO" || ! -x "$TC_RUSTC" ]]; then
    echo "Rust $TC_RUST_TOOLCHAIN is not installed completely" >&2
    return 1
  fi
  # Cargo subcommands and rustup proxies must use this same toolchain, even
  # when another Rust installation appears earlier in the inherited PATH.
  export PATH="$(dirname "$TC_CARGO"):$(dirname "$TC_RUSTC"):$PATH"
  export RUSTC="$TC_RUSTC" RUSTUP_TOOLCHAIN="$TC_RUST_TOOLCHAIN"
  rustc_release="$("$TC_RUSTC" -vV | awk '/^release:/ {print $2}')" || return 1
  cargo_version="$("$TC_CARGO" --version)" || return 1
  if [[ "$rustc_release" != "$TC_RUST_TOOLCHAIN" || ! "$cargo_version" =~ ^cargo[[:space:]]+([^[:space:]]+) || "${BASH_REMATCH[1]}" != "$TC_RUST_TOOLCHAIN" ]]; then
    echo "Expected Rust and Cargo $TC_RUST_TOOLCHAIN; found rustc $rustc_release / $cargo_version" >&2
    return 1
  fi
  echo "Verified Rust and Cargo $TC_RUST_TOOLCHAIN"
}
