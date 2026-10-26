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
check: fmt-check no-openssl lint features test test-python

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

# Refuse to release half-built code: the same deny as agora's `todo-gate`,
# plus `--all-features` so feature-gated code is linted too. Not part of
# `check`: `todo!()` is fine on branches and in PRs. Clippy never lints
# dependencies, so agora's own gate cannot see a `todo!()` in this crate;
# the release workflow runs this before `cargo publish`.
todo-gate:
    cargo clippy --all-features --all-targets -- -D clippy::todo -D clippy::unimplemented

# No openssl-sys under any feature or target (agora CLAUDE.md rule 5). The
# `no-openssl` dev-dependency (`links = "openssl"`) already fails resolution;
# this also catches openssl-sys 0.9.28-0.9.30, whose index entries lack `links`.
no-openssl:
    #!/usr/bin/env bash
    set -euo pipefail
    tree=$(cargo tree --all-features --target all -e normal,build,dev --prefix none)
    if grep -q '^openssl-sys ' <<<"$tree"; then
        echo "ERROR: openssl-sys found in dependency tree!"
        cargo tree --all-features --target all -i openssl-sys
        exit 1
    fi
    echo "No openssl-sys dependency found."

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

# The live API tests: the `#[ignore]`d `live_*` tests, on Claude Haiku 5.5
# at low effort (Steward, 2026-10-19). Paid, but cents per run. Run it
# locally before every release: agentkit CI has no secrets, by design, so
# it never runs these. The key is read from its file only
# (`$AGORA_SECRETS_DIR/anthropic_api_key`, default
# `~/Projects/agora/secrets/anthropic_api_key`), never from the
# environment. Each test prints its token usage.
live-test:
    cargo test --all-features --lib -- --ignored --exact --nocapture --test-threads 1 \
        reactor::agent::cache::tests::live_roll_breakpoints_hit_the_cache \
        reactor::anthropic::tests::live_batch_runs_server_tools

# The independent Python verifier against the shared vectors. stdlib only,
# so there is nothing to install.
test-python:
    python3 tools/test_vectors.py

# Mutate the shared vectors a few thousand ways and hold the two verifiers
# to each other on every mutant. Not part of `check` (it takes a minute);
# run it after touching a verification rule in either implementation.
fuzz count="4000" seed="1":
    python3 tools/fuzz_verifiers.py --count {{count}} --seed {{seed}}

# Rewrite the shared vectors in vectors/ from the Rust implementation.
#
# Only after changing what a vector is *meant* to say: the files are the
# contract between the two implementations, and regenerating to make a
# failing test pass is how a bug becomes a specification. Fixed key seeds
# and fixed timestamps, so a regeneration that changes nothing is a no-op
# in git.
vectors:
    cargo test --all-features govlog::vectors::regenerate_the_vectors \
        -- --ignored --exact --nocapture
    cargo test --all-features --lib \
        signing::tests::renamed_fields_vector::regenerate_the_vector \
        -- --ignored --exact --nocapture
    cargo test --all-features --lib \
        signing::tests::friendship_vector::regenerate_the_vector \
        -- --ignored --exact --nocapture
