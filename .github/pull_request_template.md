## Summary

<!-- What changed, in a sentence or two. -->

## Motivation

<!-- The problem this solves, or the failure it fixes. Link the issue if there is one. -->

## Testing

<!-- How this was verified. Name the commands that were run, and say which of
     them need a display. -->

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
```

## Scope

- [ ] The change is focused: one intent, no unrelated refactoring, no reformatting of untouched code.
- [ ] Tests are included, and they assert the property they claim rather than a substring that also survives the bug.
- [ ] Tests do not depend on the host — no reliance on the console's display, an installed X server, or the state of `/tmp`.
- [ ] No new dependency was added, or the reason is given in the summary.
- [ ] `cargo fmt`, `cargo clippy -D warnings` and `cargo test` were run, and pass.
- [ ] Documentation, the configuration sample or `CHANGELOG.md` was updated if public behaviour, a default, a flag or a command changed.
- [ ] Layout is unchanged unless the change is the layout.
- [ ] No Wayland, compositor, renderer, GPU path, wallpaper, notification, tray or animation work is included.