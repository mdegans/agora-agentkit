# Run `just` with no arguments to list recipes.
default:
    @just --list

# Point git at the repo's tracked hooks (hooks/). Run once after cloning.
install-hooks:
    git config core.hooksPath hooks
    @echo "git hooks installed: core.hooksPath = hooks/"

# The same gate the pre-commit hook runs: formatting, lints, the feature
# matrix, tests — both implementations' tests, since the Python verifier is
# only worth having if it is held to the same vectors.
check: fmt-check lint features test test-python

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

# The published-key parity check: the live chain must verify under the
# genesis key and the ROOT keys compiled into this crate, and the key the
# platform serves must be the one that walk ends on. Networked, so it is not
# part of `check` — CI runs it as its own job, where a 5G blip reads as a
# blip rather than as a code failure.
#
# A rotation needs no release here any more: the root certifies the new key
# in the chain itself, and this check follows it.
check-published-keys:
    cargo test --features agora-client --lib \
        govlog::tests::the_published_key_is_the_one_the_platform_serves \
        -- --ignored --exact --nocapture

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

# The independent Python verifier against the shared vectors. stdlib only,
# so there is nothing to install.
test-python:
    python3 tools/test_vectors.py

# Mutate the shared vectors a few thousand ways and hold the two verifiers
# to each other on every mutant. Not part of `check` (it takes a minute);
# run it after touching a verification rule in either implementation.
fuzz count="4000" seed="1":
    python3 tools/fuzz_verifiers.py --count {{count}} --seed {{seed}}

# Rewrite the shared vectors in vectors/govlog from the Rust verifier.
#
# Only after changing what a vector is *meant* to say: the files are the
# contract between the two implementations, and regenerating to make a
# failing test pass is how a bug becomes a specification. Fixed key seeds
# and fixed timestamps, so a regeneration that changes nothing is a no-op
# in git.
vectors:
    cargo test --all-features govlog::vectors::regenerate_the_vectors \
        -- --ignored --exact --nocapture
