## Summary

<!-- Tree, diagram, or short diff sketch. Explain the change, not the diff. -->

```text
src/
└── ...
```

## Evidence

- **Before:** ...
  **After:** ...

- Verification:
  `cargo fmt --all -- --check`
  `cargo test`
  `cargo clippy --all-targets -- -D warnings`
  `cargo build`

## Merge Danger

**Door:** one-way or two-way. Explain rollback.

**Blast Radius:** one word. List concrete side effects.

## Developer Checklist

- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test` passes
- [ ] `cargo build` passes
- [ ] Commits follow Conventional Commits
- [ ] Docs/README updated if behavior changed
- [ ] No secrets or credentials in the diff
