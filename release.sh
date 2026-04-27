#!/bin/sh
set -euo pipefail

# Get version from Cargo.toml
VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
TAG="v${VERSION}"

echo "Preparing release ${TAG}"

# Check if tag already exists
if git rev-parse "${TAG}" >/dev/null 2>&1; then
    echo "Error: Tag ${TAG} already exists"
    exit 1
fi

# Check for uncommitted changes (allow only Cargo.toml and Cargo.lock)
CHANGED_FILES=$(git diff --name-only HEAD)
for file in $CHANGED_FILES; do
    if [ "$file" != "Cargo.toml" ] && [ "$file" != "Cargo.lock" ]; then
        echo "Error: Unexpected uncommitted changes in ${file}"
        echo "Only Cargo.toml and Cargo.lock should be modified"
        exit 1
    fi
done

# Run tests
echo "Running cargo test..."
cargo test

# Stage and commit
echo "Committing release..."
git add Cargo.toml Cargo.lock
git commit -m "release: ${TAG}"

# Push commit
echo "Pushing to origin..."
git push origin main

# Create and push tag
echo "Creating tag ${TAG}..."
git tag "${TAG}" -m "release: ${TAG}"
git push origin "${TAG}"

echo "Release ${TAG} complete!"
