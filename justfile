# Run `just` with no arguments to list recipes.
default:
    @just --list

# Point git at the repo's tracked hooks (hooks/). Run once after cloning.
install-hooks:
    git config core.hooksPath hooks
    @echo "git hooks installed: core.hooksPath = hooks/"

# The same gate the pre-commit hook runs: formatting, lints, the feature
# matrix, tests.
check: fmt-check lint features test

# Compile every feature in isolation, plus no-default.
#
# `lint` and `test` both run `--all-features`, which cannot see this class of
# break: a module gated on one feature referencing an item gated on another
# compiles fine when everything is on, and fails for anyone enabling only the
# first. That is exactly what happened moving the `RetryAfter` impls — `retry`
# alone stopped compiling while the all-features gate stayed green.
#
# `--all-targets` because the same trap hides in test code: a `cargo check`
# without it missed a stale import that clippy then caught.
#
# Feature names are read from Cargo.toml rather than listed here, so a new
# feature is covered the day it is added instead of the day someone
# remembers this recipe.
features:
    #!/usr/bin/env bash
    set -euo pipefail
    feats=$(awk '/^\[features\]/{f=1;next} /^\[/{f=0} f && /^[a-z]/{sub(/ *=.*/,"");print}' Cargo.toml \
        | grep -v '^default$')
    printf '%-22s ' '<no-default>'
    cargo check --quiet --no-default-features --all-targets
    echo ok
    for feat in $feats; do
        printf '%-22s ' "$feat"
        cargo check --quiet --no-default-features --features "$feat" --all-targets
        echo ok
    done

# Verify formatting without modifying files.
fmt-check:
    cargo fmt --all -- --check

# Format the workspace in place.
fmt:
    cargo fmt --all

# Clippy across all features and targets, warnings treated as errors.
lint:
    cargo clippy --all-features --all-targets -- -D warnings

# Full test suite across all features.
test:
    cargo test --all-features
