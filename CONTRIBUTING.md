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

Use the repository PR template. `main` requires all five checks (`ci / fmt`, `ci / clippy`, `ci / test`, `ci / build`, `commitlint / lint`). No approval is required. Direct pushes to `main` are blocked for non-admins; admins can bypass protection and force a merge with failing checks via `gh pr merge --admin`.

Dependabot opens weekly grouped updates for Cargo and GitHub Actions. It does not auto-merge.

A weekly `cargo audit` job runs outside the PR gate and opens a `security` issue on failure.

## Releases

Releases come from `main`. Do not create tags by hand.

`release-please` opens a Release PR when releasable commits (`feat`, `fix`, or breaking) land on `main`. That PR bumps `Cargo.toml`, updates `CHANGELOG.md` and `Cargo.lock`, and is titled `chore(release): release X.Y.Z`. Merge it yourself. Nothing is tagged or published until that merge.

While the version is below `1.0.0`, `feat` bumps minor, `fix` bumps patch, and a breaking change bumps minor. Only stable `vX.Y.Z` releases are published.

Merging the Release PR makes the same workflow build Linux, macOS and Windows binaries and upload them, with `README`, `LICENSE` and `SHA256SUMS`, to the GitHub Release. It does not publish to crates.io.
