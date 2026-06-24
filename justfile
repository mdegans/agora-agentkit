# Run `just` with no arguments to list recipes.
default:
    @just --list

# Point git at the repo's tracked hooks (hooks/). Run once after cloning.
install-hooks:
    git config core.hooksPath hooks
    @echo "git hooks installed: core.hooksPath = hooks/"

# The same gate the pre-commit hook runs: formatting, lints, tests.
check: fmt-check lint test

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
