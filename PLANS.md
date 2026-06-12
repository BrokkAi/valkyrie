# Valkyrie Plans

## Summary

**Valkyrie** is an automation CLI for running agentic workloads against software repositories. It should be able to take a GitHub issue, pull request, label, branch, or CI failure and autonomously plan, execute, validate, and report on code changes.

The CLI sits above the existing agentic stack:

- **anvil**: the Agent Client Protocol agent
- **mjolnir**: the TUI Agent Client Protocol client
- **brokk**: the GUI agentic app
- **bifrost**: the tree-sitter enabled engine exposing MCP and LSP capabilities
- **valkyrie**: the automation CLI for dispatching repo work such as issues, PR fixes, CI failures, and batch maintenance tasks

Valkyrie should feel like a repo-native worker that can be invoked manually, interactively through a TUI, from CI, from cron, or by another orchestration layer.

## Product Principle: CLI and TUI First

YAML must not be the product.

Valkyrie should be fully usable through direct commands, flags, prompts, and TUI flows. A user should be able to clone a repo, run a command, answer a few interactive questions if needed, and get useful work without writing or reading a configuration file.

Configuration files can exist, but only as optional persisted defaults for repeatability, team policy, and CI ergonomics. The normal path should be:

1. Run a command.
2. See what Valkyrie intends to do.
3. Confirm or adjust through flags or the TUI.
4. Let the agent work.
5. Inspect results through CLI output, logs, or mjolnir.

A config file should be something Valkyrie can generate, explain, edit, and migrate. It should not be something users are forced to author by hand before the tool is useful.

## Goals

1. Run agentic work directly against a repository.
2. Support common automation targets:
   - Fix a GitHub issue.
   - Fix or update a pull request.
   - Respond to CI failures.
   - Sweep issues by label.
   - Perform repo maintenance tasks.
3. Make the CLI the primary interface.
4. Make TUI interaction a first-class way to inspect, approve, steer, and resume runs.
5. Use anvil as the agent execution layer.
6. Use bifrost for code intelligence through MCP and LSP.
7. Produce auditable plans, logs, diffs, commits, and summaries.
8. Be safe by default: require explicit write, push, and PR creation modes.
9. Work both locally and in headless CI environments.
10. Keep YAML optional and secondary.

## Non-Goals

1. Valkyrie is not a general-purpose chat UI.
2. Valkyrie is not a replacement for mjolnir or brokk.
3. Valkyrie should not hide agent decisions; runs must be inspectable.
4. Valkyrie should not require remote GitHub access just to inspect or plan against a local checkout, but every target must still be a concrete trigger (issue, PR, CI failure, branch) rather than a free-form prose task.
5. Valkyrie should not directly implement deep code intelligence that belongs in bifrost.
6. Valkyrie should not require users to hand-author YAML before they can run useful workloads.
7. Valkyrie should not become a configuration-file-driven workflow engine where the CLI is just a thin wrapper around YAML.

## Core Use Cases

### Fix a GitHub issue

```bash
valkyrie issue 123
```

Plan-only form:

```bash
valkyrie issue 123 --plan
```

Expected behavior:

1. Fetch issue metadata, comments, labels, linked PRs, and relevant repo context.
2. Create an execution workspace.
3. Ask the agent to produce a plan.
4. Show the plan in the terminal or TUI before meaningful writes when running interactively.
5. Apply code changes.
6. Run tests, linting, formatting, or inferred validation steps.
7. Commit changes on a branch when requested.
8. Optionally open or update a PR.
9. Post a summary back to the issue when requested.

### Review a pull request

```bash
valkyrie review 456
```

Plan-only form:

```bash
valkyrie review 456 --plan
```

Expected behavior:

1. Fetch the PR description, diff, comments, and review feedback.
2. Ask the agent to analyze the change without modifying the working tree.
3. Capture the review locally and submit it to GitHub.

Automated PR fixing (checking out the PR branch and applying changes) is
future work tracked under Milestone 4; there is no dedicated `pr` command yet.

### Sweep labeled issues

```bash
valkyrie sweep --label ready-for-agent
```

Expected behavior:

1. Find matching open issues.
2. Present the queue in the terminal or TUI.
3. Let the user include, exclude, reorder, or limit tasks with flags or interactive selection.
4. Process each issue in an isolated workspace.
5. Produce one branch and PR per issue, unless instructed otherwise.
6. Stop on budget, time, or failure limits.

### Respond to CI failures

```bash
valkyrie ci --pr 456 --fix
```

Equivalent explicit form:

```bash
valkyrie fix ci --pr 456
```

Expected behavior:

1. Fetch failing checks and logs.
2. Map failures to relevant source files.
3. Ask the agent to diagnose and patch.
4. Re-run targeted tests locally if possible.
5. Push fixes or leave a local patch based on explicit write mode.

### Local execution

Valkyrie always operates on a local repository checkout, even for GitHub
targets. A target still has to be a concrete trigger (issue, PR, and later CI
failures or branch diffs); Valkyrie does not accept free-form prose tasks,
because interactive "do this for me" prompting is the role of the agent clients
(anvil, mjolnir, brokk) rather than the automation CLI.

```bash
valkyrie issue 123 --plan
```

Expected behavior:

1. Resolve the target from local repository context plus GitHub metadata.
2. Use bifrost for code navigation and semantic context.
3. Produce a plan, diff, and validation summary.
4. Avoid remote writes unless explicitly requested.

### Interactive run steering

```bash
valkyrie issue 123 --tui
```

Expected behavior:

1. Start the run from the CLI.
2. Open or attach mjolnir-style TUI controls.
3. Show plan, files, diffs, validation output, and agent events.
4. Let the user approve, pause, resume, stop, or redirect work.
5. Preserve the run record so it can later be resumed headlessly.

## CLI Shape

### Design rules

1. The common case should be short.
2. Flags should override stored defaults.
3. Interactive prompts should fill missing information in local runs.
4. Headless runs should fail clearly when required information is missing.
5. Every action available in YAML should be available through flags or subcommands.
6. Any persisted default should be writable through the CLI.
7. The TUI should be attachable to live and completed runs.

### Top-level commands

```bash
valkyrie issue <number>
valkyrie review <number>
valkyrie ci --pr <number> [--fix]
valkyrie sweep [filters]
valkyrie patrol [repo]
valkyrie status [run-id]
valkyrie logs <run-id>
valkyrie diff <run-id>
valkyrie replay <run-id>
valkyrie resume <run-id>
valkyrie attach <run-id>
valkyrie tui [run-id]
valkyrie defaults get [key]
valkyrie defaults set <key> <value>
valkyrie defaults unset <key>
valkyrie defaults export
valkyrie doctor
```

### Target examples

```bash
valkyrie issue 123
valkyrie issue 123 --plan
valkyrie issue 123 --commit
valkyrie issue 123 --commit --push --open-pr
valkyrie review 456
valkyrie ci --pr 456 --fix
valkyrie sweep --label ready-for-agent --max 5
valkyrie patrol --label bug --max 5
valkyrie issue 123 --plan
valkyrie attach latest
valkyrie tui latest
```

### Execution flags

```bash
--repo <path-or-url>
--branch <name>
--base <branch>
--agent <name>
--model <name>
--profile <profile>
--dry-run
--no-write
--write
--commit
--push
--open-pr
--update-pr
--post-comment
--max-iterations <n>
--max-files <n>
--timeout <duration>
--budget <amount>
--validate <command>
--skip-validation
--tui
--json
--verbose
```

### Defaults commands

Stored defaults should be managed through commands, not hand-edited files.

```bash
valkyrie defaults set repo.base main
valkyrie defaults set validation.command "cargo test"
valkyrie defaults set validation.command "cargo fmt --check"
valkyrie defaults set write.push false
valkyrie defaults get
valkyrie defaults export
```

The CLI should make it obvious where a default came from:

```text
Validation commands:
  cargo test         from repo default
  cargo fmt --check  from repo default

Write policy:
  commit: false      default
  push: false        default
  open_pr: false     default
```

## Configuration and Stored Defaults

Valkyrie should work without a config file.

The precedence order should be:

1. Explicit CLI flags.
2. Interactive TUI choices for the current run.
3. Environment variables for CI/secrets.
4. Repo-local stored defaults.
5. User-global stored defaults.
6. Built-in defaults.

Config files are allowed as an implementation detail and for CI reproducibility, but they should not be the primary UX.

Preferred user flow:

```bash
valkyrie issue 123 --commit
```

If Valkyrie needs missing information, it can ask:

```text
No validation command found.

Choose validation:
  1. cargo test
  2. cargo test --workspace
  3. npm test
  4. skip for this run
  5. enter custom command

Selection: 1

Save this as a repo default? [y/N]
```

If the user saves defaults, Valkyrie may persist them internally or in a generated file. The user should not need to care unless they ask.

Optional generated file:

```yaml
# .valkyrie/defaults.yaml
# Generated by `valkyrie defaults export`.
# Prefer `valkyrie defaults set ...` over hand-editing this file.
repo:
  base: main
  worktree_root: .valkyrie/worktrees

agent:
  provider: anvil
  profile: default

engine:
  provider: bifrost
  mcp: true
  lsp: true

validation:
  commands:
    - cargo test
    - cargo fmt --check

write:
  commit: false
  push: false
  open_pr: false
  post_comment: false

limits:
  max_iterations: 8
  max_files_changed: 25
  timeout_minutes: 60
```

Rules for config:

1. No config required for normal use.
2. No hand-authored YAML required for MVP.
3. Every config value must be settable and inspectable from the CLI.
4. Generated config must include comments explaining how to change values with commands.
5. YAML should be accepted for teams and CI, but it should feel like an export format, not the primary interface.
6. CLI flags always win.
7. TUI choices should be able to persist defaults when the user explicitly chooses to save them.

## Architecture

```text
+-------------------+
|     valkyrie      |
| CLI + TUI control |
+---------+---------+
          |
          v
+-------------------+        +-------------------+
|   target loader   |------->| GitHub / local fs |
+---------+---------+        +-------------------+
          |
          v
+-------------------+
| defaults resolver |
| flags > TUI > env |
+---------+---------+
          |
          v
+-------------------+
| execution planner |
+---------+---------+
          |
          v
+-------------------+        +-------------------+
|      anvil        |<------>|      bifrost      |
| ACP agent runtime |        | MCP + LSP engine  |
+---------+---------+        +-------------------+
          |
          v
+-------------------+
| workspace manager |
+---------+---------+
          |
          v
+-------------------+
| validation runner |
+---------+---------+
          |
          v
+-------------------+
| reporter / writer |
+-------------------+
```

## Main Components

### Target Loader

Responsible for resolving what Valkyrie should work on.

Inputs:

- Local prompt
- GitHub issue
- GitHub PR
- GitHub label query
- CI failure
- Branch diff
- Repository maintenance task

Outputs:

- Normalized task object
- Relevant metadata
- Initial context bundle

### Defaults Resolver

Responsible for merging explicit choices, interactive answers, stored defaults, and built-in defaults.

Responsibilities:

- Treat CLI flags as authoritative.
- Support repo-local and user-global defaults.
- Explain where each effective value came from.
- Persist defaults only when the user asks.
- Export YAML or another machine-readable format only as an optional artifact.

### Workspace Manager

Responsible for isolated execution.

Responsibilities:

- Clone or reuse repo.
- Create worktree.
- Check out target branch.
- Track dirty state.
- Apply patches.
- Commit changes.
- Clean up or preserve workspaces.

### Planner

Responsible for converting a target into a bounded work plan.

Plan should include:

- Problem statement
- Relevant files
- Proposed changes
- Validation steps
- Risks
- Stop conditions

### Agent Runner

Responsible for invoking anvil.

Responsibilities:

- Provide target context.
- Connect bifrost tools through MCP/LSP.
- Stream agent events.
- Enforce iteration, budget, and file-change limits.
- Persist execution trace.

### TUI Controller

Responsible for attaching human control to a run.

Responsibilities:

- Show the current plan.
- Show agent activity and tool calls.
- Show changed files and diffs.
- Show validation output.
- Let the user pause, resume, approve, reject, or redirect work.
- Let the user save useful choices as defaults.
- Attach to live runs and inspect completed runs.

### Validation Runner

Responsible for checking whether the change works.

Validation sources:

- Explicit `--validate` flags
- Stored defaults
- Language-specific inferred defaults
- CI-derived commands
- Targeted test selection from changed files
- Agent-proposed validation steps

### Reporter

Responsible for producing human-readable and machine-readable run outputs.

Outputs:

- Markdown summary
- JSON run metadata
- Git diff
- Commit message
- PR body
- Issue or PR comment
- Failure report

## Run Lifecycle

1. **Resolve target**
   - Parse CLI arguments.
   - Load repo and target metadata.
   - Determine execution mode.

2. **Resolve defaults**
   - Merge flags, TUI choices, environment variables, stored defaults, and built-in defaults.
   - Ask interactive questions only when appropriate.
   - Explain effective settings when running with `--verbose`, `--dry-run`, or `--plan`.

3. **Prepare workspace**
   - Ensure clean worktree or create isolated worktree.
   - Check out branch.
   - Install or verify tool dependencies.

4. **Gather context**
   - Load issue or PR text.
   - Read comments and review threads.
   - Ask bifrost for relevant symbols, files, references, diagnostics, and repo structure.

5. **Plan**
   - Produce a concise plan before modifying files.
   - Show the plan in CLI/TUI.
   - Store the plan in the run log.

6. **Execute**
   - Let the agent edit files.
   - Keep an event stream of tool calls, file changes, and decisions.
   - Enforce limits.

7. **Validate**
   - Run validation commands.
   - If validation fails, allow bounded repair loops.

8. **Finalize**
   - Generate summary.
   - Produce patch or commit.
   - Optionally push branch, open PR, update PR, or comment.

9. **Record**
   - Store run metadata under `.valkyrie/runs/<run-id>`.
   - Make the run replayable, resumable, inspectable, and attachable from the TUI.

## Write Modes

### no-write

Planning and analysis only. No file changes.

```bash
valkyrie issue 123 --plan
```

### local-patch

Apply changes locally but do not commit.

```bash
valkyrie issue 123
```

### commit

Apply changes and commit locally.

```bash
valkyrie issue 123 --commit
```

### push

Commit and push branch.

```bash
valkyrie issue 123 --commit --push
```

### pr

Commit, push, and open or update a pull request.

```bash
valkyrie issue 123 --commit --push --open-pr
```

## Safety Model

Valkyrie should be useful in automation without being reckless.

Default behavior:

- Do not push unless `--push` is present or an explicit stored default allows it.
- Do not open PRs unless `--open-pr` is present or an explicit stored default allows it.
- Do not post comments unless `--post-comment` is present or an explicit stored default allows it.
- Do not modify protected branches directly.
- Stop when file-change limits are exceeded.
- Stop when validation repeatedly fails.
- Store all plans and logs.
- Make every remote write explicit and auditable.
- In interactive mode, show the planned side effects before executing them.

## GitHub Integration

Initial GitHub support should include:

- Fetch issue by number.
- Fetch PR by number.
- Fetch issue comments.
- Fetch PR review comments.
- Fetch unresolved review threads if available.
- Fetch check runs and failing logs.
- Create branch.
- Push branch.
- Open PR.
- Update PR body.
- Post issue or PR comment.
- Apply labels for lifecycle state.

Suggested label workflow:

```text
ready-for-agent -> agent-in-progress -> agent-done
ready-for-agent -> agent-in-progress -> agent-blocked
```

## Output Artifacts

Each run should write a directory like:

```text
.valkyrie/runs/2026-06-03T12-00-00Z-issue-123/
  target.json
  effective-settings.json
  plan.md
  events.jsonl
  diff.patch
  validation.md
  summary.md
  result.json
```

The run directory is the audit trail. It is also what mjolnir or brokk can open to inspect the work.

## Result States

```text
planned
waiting_for_approval
changed
validated
committed
pushed
pr_opened
pr_updated
commented
blocked
failed
cancelled
paused
resumed
```

## MVP

The first useful version should support:

1. Local repo execution.
2. GitHub issue target.
3. GitHub PR review target.
4. anvil agent invocation.
5. bifrost MCP/LSP context.
6. CLI-provided validation commands.
7. Inferred validation commands for common stacks.
8. Stored defaults managed by CLI commands.
9. Local patch output.
10. Optional commit.
11. Markdown and JSON summaries.
12. Basic TUI attach or run inspection.

MVP commands:

```bash
valkyrie issue 123 --repo .
valkyrie review 456 --repo .
valkyrie issue 123 --plan --repo .
valkyrie defaults set validation.command "cargo test"
valkyrie status <run-id>
valkyrie logs <run-id>
valkyrie diff <run-id>
valkyrie attach <run-id>
```

## Milestones

### Milestone 1: Local target runner

Status: [~] In progress

- [x] Parse CLI args.
- [x] Resolve defaults without requiring config.
- [x] Create run directory.
- [ ] Invoke anvil against local repo.
- [x] Write plan, logs, diff, and summary.

### Milestone 2: CLI-managed defaults

Status: [~] In progress

- [x] Implement `defaults get`.
- [x] Implement `defaults set`.
- [x] Implement `defaults unset`.
- [x] Implement `defaults export`.
- [x] Show effective settings and their sources.
- [x] Support repo-local and user-global defaults.

### Milestone 3: GitHub issue support

Status: [~] In progress

- [x] Resolve `issue <number>` targets.
- [x] Fetch issue title, body, labels, and comments.
- [x] Create task context from issue metadata.
- [x] Generate local patch.

### Milestone 4: PR support

Status: [ ] Not started

- [ ] Resolve `pr <number>` targets.
- [ ] Check out PR branch.
- [ ] Read PR comments and review feedback.
- [ ] Apply fixes locally.
- [ ] Summarize changes for PR comment.

### Milestone 5: Validation loops

Status: [~] In progress

- [x] Run validation commands from flags, inferred defaults, or stored defaults.
- [ ] Feed failures back into the agent.
- [ ] Bound repair loops with iteration limits.
- [x] Produce validation report.

### Milestone 6: TUI attach

Status: [ ] Not started

- [ ] Attach to live run.
- [ ] Inspect completed run.
- [ ] Show plan, logs, diffs, and validation.
- [ ] Pause, resume, cancel, or approve actions.
- [ ] Save interactive choices as defaults when requested.

### Milestone 7: Remote write support

Status: [ ] Not started

- [ ] Commit changes.
- [ ] Push branches.
- [ ] Open PRs.
- [ ] Update PRs.
- [ ] Post comments.
- [ ] Add lifecycle labels.

### Milestone 8: Batch automation

Status: [ ] Not started

- [ ] Implement `sweep`.
- [ ] Select issues by labels and filters.
- [ ] Preview the queue in CLI/TUI.
- [ ] Run isolated workspaces.
- [ ] Enforce concurrency, budget, and failure limits.

### Milestone 9: CI failure repair

Status: [ ] Not started

- [ ] Fetch failed checks.
- [ ] Parse logs.
- [ ] Infer validation commands.
- [ ] Patch and revalidate.

## Milestone Progress

Legend:

- `[ ]` Not started or not yet verified.
- `[~]` In progress.
- `[x]` Done.
- `[!]` Blocked.

Progress markers in this section reflect the plan status only when they are intentionally updated. If implementation status is uncertain, leave the item unchecked rather than implying completion.

## Open Questions

1. Should the default project name be `valkyrie`, or should the CLI be shorter, such as `rune`?
2. Should Valkyrie own GitHub integration directly, or should GitHub be an MCP server/tool exposed to the agent?
3. Should remote writes be controlled only by CLI flags, or also by repository policy defaults?
4. Should `sweep` process issues serially by default, or support parallel workers from the beginning?
5. Should run logs be optimized for human debugging, machine replay, or both?
6. Should brokk be able to open and inspect a Valkyrie run directory?
7. Should mjolnir be able to attach to a live Valkyrie run?
8. Should bifrost provide task-specific context packs for issues and PRs?
9. Should stored defaults live in `.valkyrie/defaults.yaml`, platform config dirs, Git config, SQLite, or a combination?
10. Should the default interactive mode be terminal prompts, a full TUI, or prompts that can escalate into TUI?

## Design Principles

1. **CLI-first**: every common workflow should be expressible as a clear command.
2. **TUI-native**: users should be able to inspect, steer, approve, and resume work interactively.
3. **YAML-optional**: configuration files may exist, but the user should not need to author one.
4. **Repo-native**: everything starts from a repository and leaves behind normal Git artifacts.
5. **Auditable**: every run has a plan, event log, diff, validation result, and summary.
6. **Composable**: the CLI should work locally, in CI, and under higher-level orchestration.
7. **Safe by default**: remote side effects require explicit permission.
8. **Agent-agnostic at the edges**: anvil is the first-class agent runtime, but boundaries should stay clean.
9. **Bifrost-powered**: code intelligence should come from bifrost rather than being reimplemented.
10. **Human-overridable**: users should be able to inspect, resume, replay, or take over runs.

## Example End-to-End Flow

```bash
valkyrie issue 123 \
  --commit \
  --push \
  --open-pr \
  --post-comment
```

Expected final output:

```text
Run: 2026-06-03T12-00-00Z-issue-123
Target: issue #123
State: pr_opened
Branch: valkyrie/issue-123-fix-parser-crash
Commit: abc1234
PR: #789
Validation: passed
Summary: .valkyrie/runs/2026-06-03T12-00-00Z-issue-123/summary.md
```

## Example First-Run Flow

```bash
valkyrie issue 123
```

Possible interaction:

```text
Repo: /Users/ryan/src/example
Target: issue #123
Mode: local patch

I found these likely validation commands:
  1. cargo test
  2. cargo fmt --check
  3. cargo clippy --all-targets --all-features

Run all three? [Y/n]
Save these as repo defaults? [y/N]

Plan is ready. Proceed with edits? [Y/n]
```

Equivalent non-interactive command:

```bash
valkyrie issue 123 \
  --validate "cargo test" \
  --validate "cargo fmt --check" \
  --validate "cargo clippy --all-targets --all-features" \
  --commit
```

## Initial Implementation Sketch

Suggested crate/module layout:

```text
valkyrie/
  crates/
    valkyrie-cli/
    valkyrie-core/
    valkyrie-defaults/
    valkyrie-github/
    valkyrie-workspace/
    valkyrie-agent/
    valkyrie-validation/
    valkyrie-reporting/
    valkyrie-tui/
  docs/
    plans.md
  examples/
    generated-defaults.yaml
```

Core interfaces:

```rust
trait TargetResolver {
    fn resolve(&self, input: TargetInput) -> Result<Target>;
}

trait DefaultsResolver {
    fn resolve(&self, cli: CliArgs, env: Env, repo: RepoContext) -> Result<EffectiveSettings>;
}

trait AgentRunner {
    fn run(&self, task: AgentTask, workspace: Workspace) -> Result<AgentResult>;
}

trait ValidationRunner {
    fn validate(&self, workspace: Workspace, plan: ValidationPlan) -> Result<ValidationResult>;
}

trait Reporter {
    fn write(&self, run: RunRecord) -> Result<ReportPaths>;
}

trait TuiController {
    fn attach(&self, run_id: RunId) -> Result<()>;
}
```

## Near-Term Next Steps

1. Keep `valkyrie` as the working CLI name.
2. Define the `Target`, `RunRecord`, and `EffectiveSettings` data models.
3. Implement the direct `valkyrie issue 123` shortcut.
4. Implement CLI-managed defaults before any hand-authored YAML requirement.
5. Add GitHub issue resolution.
6. Wire anvil execution with bifrost context.
7. Persist run artifacts.
8. Add validation command execution.
9. Add basic TUI attach/inspect.
10. Add commit support.
11. Add PR creation and commenting behind explicit flags.

