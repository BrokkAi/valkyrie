# AGENTS.md

Instructions for agents working on this repository.

## Project context

Valkyrie is a Rust CLI for agentic automation. It is intended to plan, execute, validate, and report software changes in repositories. The project should remain CLI/TUI first, safe by default, and produce inspectable run records.

## Language policy

All code, comments, documentation, CLI text, error messages, tests, and generated project files should be written in English unless a task explicitly requires another language.

## General expectations

- Read `PLANS.md` before changing product behavior or CLI shape.
- Keep changes small, focused, and easy to review.
- Preserve existing behavior unless the requested task explicitly changes it.
- Document non-obvious decisions in code comments or in the final change summary.
- Do not introduce dependencies without a clear justification.
- Prefer boring, maintainable code over clever abstractions.

## Required Rust best practices

Apply standard Rust best practices to every change:

- Run `cargo fmt` before finishing.
- Run `cargo clippy --all-targets --all-features -- -D warnings` and fix warnings instead of suppressing them.
- Run `cargo test` and add relevant tests for new behavior or bug fixes.
- Prefer simple, idiomatic, explicit Rust over premature abstractions.
- Avoid `unwrap()` and `expect()` in production code when errors can be propagated or handled cleanly.
- Use `Result` with useful error messages for operations that can fail.
- Keep functions short and cohesive; extract helpers when a function becomes hard to test.
- Avoid unnecessary allocations and clones, but do not sacrifice readability without a measured reason.
- Prefer strong types over free-form strings when they clarify invariants.
- Keep internal APIs testable without relying on the filesystem, network, or external commands when possible.

## Tests

This repository currently has limited test coverage. Changes should improve that situation whenever reasonable.

- Add unit tests for pure logic: parsing, normalization, markdown rendering, default selection, and flag validation.
- Add regression tests for every fixed bug.
- For CLI behavior, prefer testable functions that are separated from raw argument parsing and side effects.
- Tests must not depend on external network access or nondeterministic local state.
- If a change does not add tests, explain why in the final summary.

Recommended minimum validation commands:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## Clippy and rustfmt quality bar

- Submitted code must be `rustfmt` compliant.
- Clippy warnings should be treated as errors.
- Do not add `#[allow(...)]` without a clear local justification.
- If Clippy reveals a structural issue, prefer fixing the design over bypassing the lint.

## Safety and side effects

Valkyrie may orchestrate changes across repositories and remote services, so agents must be careful:

- Do not enable remote writes, pushes, PR creation, or remote comments without an explicit option.
- Keep safe modes as the default.
- Be careful with shell commands constructed from user input.
- Avoid exposing secrets in logs, reports, or error messages.

## Contribution style

Before finishing a task, provide a summary that includes:

1. Modified files.
2. Functional changes.
3. Tests and validation commands that were run.
4. Missing tests or remaining risks, if any.
