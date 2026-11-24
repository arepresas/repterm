# Contributing

## Commits

Use Conventional Commits, no emoji:

```text
type(scope): subject
```

Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`.

Scopes: `app`, `chat`, `tools`, `llm`, `shell`, `tabs`, `git`, `ci`, `deps`, `docs`, `test`, `release`. Scope is optional. Header limit is 100 characters.

Examples:

```text
feat(tools): add scoped file reads
fix(shell): keep bang prompts out of zsh expansion
ci: add release workflow
```

`commitlint` checks PR titles and commit messages in CI.

## Local checks

Run the same gates as CI before opening a PR:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build
```

The toolchain is pinned in `rust-toolchain.toml` (1.99.1 with rustfmt and clippy).

## Pull requests

Use the repository PR template. `main` requires all five checks (`ci / fmt`, `ci / clippy`, `ci / test`, `ci / build`, `commitlint / lint`). No approval is required. Direct pushes to `main` are blocked.

Dependabot opens weekly grouped updates for Cargo and GitHub Actions. It does not auto-merge.

A weekly `cargo audit` job runs outside the PR gate and opens a `security` issue on failure.

## Releases

1. Bump `version` in `Cargo.toml`.
2. Commit with `chore(release): bump version to X.Y.Z`.
3. Create and push an annotated tag:

```bash
git tag -a vX.Y.Z -m "Release vX.Y.Z"
git push origin main
git push origin vX.Y.Z
```

Pushing the tag triggers `release.yml`: it generates release notes with git-cliff, builds release binaries for Linux, macOS and Windows, and publishes a GitHub Release with checksums. The in-repo `CHANGELOG.md` is updated separately in a normal PR. Tags containing `-beta` or `-rc` are marked as prereleases.
