# Agent conventions

## Commits

- **Do not add a co-authored footer** to commit messages (no
  `Co-Authored-By:` / `Co-authored-by:` trailer, no "Generated with" line).
- **Always sign commits** — use `git commit -S` (signed commit). Do not create
  unsigned commits.
- **Never push without asking** — do not run `git push` unless the user has
  explicitly requested it.

## Code style

- Use a modern, idiomatic Rust style.
- Practice TDD when appropriate.
- Prefer property-based tests (proptests) when possible.
- Write clear, self-documenting code: expressive variable and function names and
  modular design are far better than any comment.
- Comments explain the *why* behind the code, not the *what* or *how*.
- Use comments sparingly, only where they genuinely document critical context,
  invariants, business logic, or unavoidable architectural constraints.
- Keep comments brief.
- Update comments during refactoring to avoid stale or wrong comments.
- Use docstrings (`///`) for systematically generating public API documentation.
- Use idiomatic doc headers: `# Safety`, `# Examples`, `# Errors`, and `# Panics`.
- Include executable examples where appropriate; `cargo test` validates that the
  code in docs still works.
- Link internal items via intra-doc links, e.g. `[MyStruct]`.
