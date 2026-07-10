#!/bin/sh
set -eu

# Automated release via cocogitto.
#
# `cog bump --auto` inspects the conventional commits since the latest tag,
# picks the next semver version, and then (per cog.toml):
#   - pre_bump_hooks:  bump the version in Cargo.toml + run `cargo check`
#   - creates the bump commit and the `v<version>` tag
#   - post_bump_hooks: push the commit and tag to origin
#
# Do NOT bump Cargo.toml by hand — cog owns the version now.

# Releases are cut from main.
BRANCH=$(git rev-parse --abbrev-ref HEAD)
if [ "$BRANCH" != "main" ]; then
    echo "Error: releases must be cut from main (currently on ${BRANCH})"
    exit 1
fi

# cog bump requires a clean tree so it can create the bump commit itself.
if ! git diff --quiet HEAD || ! git diff --cached --quiet; then
    echo "Error: working tree is dirty; commit or stash changes first"
    echo "cog manages the version — do not bump Cargo.toml manually"
    exit 1
fi

echo "Next version:"
cog bump --auto --dry-run

echo "Bumping, committing, tagging, and pushing..."
cog bump --auto

echo "Release complete!"
