use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1).collect::<Vec<_>>();

    if args.is_empty() {
        print_help();
        return Ok(());
    }

    let command = args.remove(0);
    match command.as_str() {
        "issue" => command_issue(args),
        "pr" => command_pr(args),
        "review" => command_review(args),
        "defaults" => command_defaults(args),
        "status" => command_show_artifact(args, "result.json"),
        "logs" => command_show_artifact(args, "events.jsonl"),
        "diff" => command_show_artifact(args, "diff.patch"),
        "doctor" => command_doctor(),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => Err(format!(
            "unknown command `{other}`. Run `vk help` for usage."
        )),
    }
}

fn execute_target_run(args: Vec<String>, plan_only: bool) -> Result<(), String> {
    let parsed = ParsedRun::parse(args, plan_only)?;
    let repo = resolve_repo(&parsed.repo)?;
    let defaults = DefaultsResolver::load(&repo)?;
    let settings = EffectiveSettings::from_inputs(&parsed, &defaults);
    let target_context = resolve_target_context(&parsed.target, &repo);
    let run_id = make_run_id(&parsed.target.slug());
    let run_dir = repo.join(".valkyrie").join("runs").join(&run_id);
    fs::create_dir_all(&run_dir).map_err(|error| {
        format!(
            "cannot create run directory `{}`: {error}",
            run_dir.display()
        )
    })?;

    let plan = render_plan(&parsed, &settings, &target_context, plan_only);
    write_text(
        run_dir.join("target.json"),
        &render_target_json(&parsed.target, &repo),
    )?;
    write_text(run_dir.join("effective-settings.json"), &settings.to_json())?;
    write_text(run_dir.join("context.md"), &target_context.to_markdown())?;
    if let Some(metadata_json) = &target_context.metadata_json {
        let metadata_file = match &parsed.target {
            Target::Issue(_) => "issue.json",
            Target::PullRequest(_) => "pr.json",
        };
        write_text(run_dir.join(metadata_file), metadata_json)?;
    }
    write_text(run_dir.join("plan.md"), &plan)?;
    write_text(
        run_dir.join("events.jsonl"),
        &format!(
            "{{\"event\":\"run_created\",\"run_id\":\"{}\",\"mode\":\"{}\"}}\n",
            escape_json(&run_id),
            if plan_only {
                "plan"
            } else {
                settings.write_mode.as_str()
            }
        ),
    )?;

    if parsed.json {
        println!("{}", render_created_json(&run_id, &run_dir, plan_only));
    } else {
        println!("Run created: {run_id}");
        println!("Artifacts: {}", run_dir.display());
        println!();
        println!("{plan}");
        if parsed.verbose || parsed.dry_run || plan_only {
            println!();
            println!("{}", settings.render_human());
        }
    }

    if plan_only || settings.write_mode == WriteMode::NoWrite {
        write_text(run_dir.join("diff.patch"), "")?;
        write_text(
            run_dir.join("validation.md"),
            "# Validation

Validation was not run because this was a planning/no-write run.
",
        )?;
        write_text(
            run_dir.join("summary.md"),
            "# Summary

Planning completed. No files were modified by Valkyrie.
",
        )?;
        write_text(
            run_dir.join("result.json"),
            &format!(
                "{{\n  \"run_id\": \"{}\",\n  \"state\": \"planned\",\n  \"run_dir\": \"{}\"\n}}\n",
                escape_json(&run_id),
                escape_json(&run_dir.display().to_string())
            ),
        )?;
        return Ok(());
    }

    if settings.write_mode.requires_clean_worktree() {
        ensure_clean_worktree(&repo)?;
    }

    let status_before = git_output(&repo, &["status", "--porcelain"])?;
    let diff_before = git_output(&repo, &["diff", "--"])?;
    let agent = run_agent(&repo, &run_dir, &parsed, &settings, &target_context, &plan)?;
    let status_after_agent = git_output(&repo, &["status", "--porcelain"])?;
    let diff = git_output(&repo, &["diff", "--"])?;
    write_text(run_dir.join("diff.patch"), &diff)?;

    let validation = run_validation(&repo, &settings)?;
    write_text(run_dir.join("validation.md"), &validation.to_markdown())?;
    let changed = diff != diff_before || status_after_agent != status_before;

    let write_actions = if agent.success && !validation.failed() {
        run_write_actions(
            &repo,
            &run_dir,
            &parsed,
            &settings,
            &target_context,
            changed,
            &validation,
        )?
    } else {
        WriteActionReport::skipped("agent or validation failed before write actions")
    };
    write_text(
        run_dir.join("write-actions.md"),
        &write_actions.to_markdown(),
    )?;

    let state = if !agent.success || validation.failed() || write_actions.failed() {
        "failed"
    } else if changed {
        if write_actions.has_remote_effect() {
            "published"
        } else if write_actions.committed() {
            "committed"
        } else {
            "changed"
        }
    } else if validation.skipped {
        "completed"
    } else {
        "validated"
    };

    let agent_state = if agent.invoked {
        if agent.success { "completed" } else { "failed" }
    } else {
        "skipped"
    };
    write_text(
        run_dir.join("summary.md"),
        &format!(
            "# Summary\n\nValkyrie created an inspectable run record and {}.\n\n- Agent: `{}`.\n- Files changed: {}.\n- Validation state: `{state}`.\n- Commit: {}.\n- Push: {}.\n- PR/comment: {}.\n\nSee `agent.md`, `diff.patch`, `validation.md`, and `write-actions.md` in this run directory for details.\n",
            if agent.invoked {
                "invoked the configured agent command"
            } else {
                "did not invoke an agent because no agent command was configured"
            },
            agent_state,
            if changed { "yes" } else { "no" },
            write_actions.commit_summary(),
            write_actions.push_summary(),
            write_actions.remote_summary(),
        ),
    )?;
    write_text(
        run_dir.join("result.json"),
        &format!(
            r#"{{
  "run_id": "{}",
  "state": "{}",
  "run_dir": "{}",
  "agent_invoked": {},
  "agent_success": {},
  "changed": {},
  "validation": {{ "skipped": {}, "failed": {} }},
  "write_actions": {}
}}
"#,
            escape_json(&run_id),
            state,
            escape_json(&run_dir.display().to_string()),
            agent.invoked,
            agent.success,
            changed,
            validation.skipped,
            validation.failed(),
            write_actions.to_json()
        ),
    )?;

    if !parsed.json {
        println!();
        println!("Run finished: {state}");
        println!("Artifacts: {}", run_dir.display());
        println!("Files changed: {}", if changed { "yes" } else { "no" });
        println!("Commit: {}", write_actions.commit_summary());
        println!("Push: {}", write_actions.push_summary());
        println!("PR/comment: {}", write_actions.remote_summary());
    }

    if agent.invoked && !agent.success {
        return Err(format!(
            "agent command failed for run `{}`. See `{}`",
            run_id,
            run_dir.join("agent.md").display()
        ));
    }

    if validation.failed() {
        return Err(format!(
            "validation failed for run `{}`. See `{}`",
            run_id,
            run_dir.join("validation.md").display()
        ));
    }

    if write_actions.failed() {
        return Err(format!(
            "write actions failed for run `{}`. See `{}`",
            run_id,
            run_dir.join("write-actions.md").display()
        ));
    }

    Ok(())
}

fn command_issue(args: Vec<String>) -> Result<(), String> {
    if args.is_empty() {
        return Err("usage: vk issue <number> [--repo <path>] [--plan]".to_string());
    }

    let mut rewritten = vec!["issue".to_string(), args[0].clone()];
    let mut plan_only = false;
    for arg in args.into_iter().skip(1) {
        if arg == "--plan" {
            plan_only = true;
        } else {
            rewritten.push(arg);
        }
    }
    execute_target_run(rewritten, plan_only)
}

fn command_pr(args: Vec<String>) -> Result<(), String> {
    if args.is_empty() {
        return Err("usage: vk pr <number> [--repo <path>] [--fix] [--plan]".to_string());
    }

    let mut rewritten = vec!["pr".to_string(), args[0].clone()];
    let mut plan_only = false;
    for arg in args.into_iter().skip(1) {
        match arg.as_str() {
            "--plan" => plan_only = true,
            "--fix" => {}
            _ => rewritten.push(arg),
        }
    }
    execute_target_run(rewritten, plan_only)
}

fn command_review(args: Vec<String>) -> Result<(), String> {
    let parsed = ParsedReview::parse(args)?;
    let repo = resolve_repo(&parsed.repo)?;
    let target_context = resolve_pr_context(&parsed.number, &repo);
    let diff = gh_pr_diff(&parsed.number, &repo);

    let run_id = make_run_id(&format!("review-pr-{}", parsed.number));
    let run_dir = repo.join(".valkyrie").join("runs").join(&run_id);
    fs::create_dir_all(&run_dir).map_err(|error| {
        format!(
            "cannot create run directory `{}`: {error}",
            run_dir.display()
        )
    })?;

    let target = Target::PullRequest(parsed.number.clone());
    write_text(
        run_dir.join("target.json"),
        &render_target_json(&target, &repo),
    )?;
    write_text(run_dir.join("context.md"), &target_context.to_markdown())?;
    if let Some(metadata_json) = &target_context.metadata_json {
        write_text(run_dir.join("pr.json"), metadata_json)?;
    }
    match &diff {
        Ok(diff) => write_text(run_dir.join("pr.diff"), diff)?,
        Err(error) => write_text(
            run_dir.join("pr.diff"),
            &format!("Diff could not be fetched: {error}\n"),
        )?,
    }

    let review_path = run_dir.join("review.md");
    let plan = render_review_plan(&parsed, &target_context);
    write_text(run_dir.join("plan.md"), &plan)?;
    write_text(
        run_dir.join("events.jsonl"),
        &format!(
            "{{\"event\":\"review_created\",\"run_id\":\"{}\",\"pr\":\"{}\"}}\n",
            escape_json(&run_id),
            escape_json(&parsed.number)
        ),
    )?;

    if !parsed.json {
        println!("Review created: {run_id}");
        println!("Artifacts: {}", run_dir.display());
        println!();
        println!("{plan}");
    }

    if parsed.plan_only {
        write_text(
            run_dir.join("summary.md"),
            "# Summary\n\nReview planning completed. The PR was not analyzed by an agent.\n",
        )?;
        write_text(
            run_dir.join("result.json"),
            &format!(
                "{{\n  \"run_id\": \"{}\",\n  \"state\": \"planned\",\n  \"run_dir\": \"{}\"\n}}\n",
                escape_json(&run_id),
                escape_json(&run_dir.display().to_string())
            ),
        )?;
        return Ok(());
    }

    let defaults = DefaultsResolver::load(&repo)?;
    let agent_command = defaults
        .get("agent.command")
        .or_else(detect_agent_command)
        .ok_or_else(|| {
            "uvx brokk acp is required to review a PR but uvx was not found. Install uv or set VALKYRIE_AGENT_COMMAND/agent.command to an ACP agent command.".to_string()
        })?;

    let relative_review_path = review_path
        .strip_prefix(&repo)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| review_path.display().to_string());
    let prompt = render_review_prompt(
        &parsed,
        &target_context,
        diff.as_deref().ok(),
        &relative_review_path,
    );
    let agent = invoke_acp_agent(&repo, &run_dir, target.kind(), &agent_command, &prompt)?;

    if fs::read_to_string(&review_path)
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        write_text(
            review_path.clone(),
            &format!(
                "# PR #{} Review\n\nThe agent did not produce a review file. See `agent.md` for details.\n",
                parsed.number
            ),
        )?;
    }
    let review = fs::read_to_string(&review_path).unwrap_or_default();

    let post = if parsed.post_comment && agent.success {
        post_pr_review(&repo, &parsed, &review_path)?
    } else if parsed.post_comment {
        WriteActionOutcome::requested_skipped("agent failed before posting the review")
    } else {
        WriteActionOutcome::skipped("not requested")
    };
    write_text(run_dir.join("write-actions.md"), &post.to_markdown())?;

    let state = review_state(agent.success, &post);

    write_text(
        run_dir.join("summary.md"),
        &format!(
            "# Summary\n\nValkyrie reviewed GitHub PR #{}.\n\n- Agent: `{}`.\n- Decision: `{}`.\n- Posted comment: {}.\n\nSee `review.md`, `pr.diff`, and `agent.md` in this run directory for details.\n",
            parsed.number,
            if agent.success { "completed" } else { "failed" },
            parsed.decision.as_str(),
            post.summary(),
        ),
    )?;
    write_text(
        run_dir.join("result.json"),
        &format!(
            "{{\n  \"run_id\": \"{}\",\n  \"state\": \"{}\",\n  \"run_dir\": \"{}\",\n  \"agent_success\": {},\n  \"decision\": \"{}\",\n  \"posted\": {}\n}}\n",
            escape_json(&run_id),
            state,
            escape_json(&run_dir.display().to_string()),
            agent.success,
            parsed.decision.as_str(),
            post.success
        ),
    )?;

    if parsed.json {
        println!(
            "{{\n  \"run_id\": \"{}\",\n  \"state\": \"{}\",\n  \"run_dir\": \"{}\"\n}}",
            escape_json(&run_id),
            state,
            escape_json(&run_dir.display().to_string())
        );
    } else {
        println!();
        println!("Review finished: {state}");
        println!("Review: {}", review_path.display());
        println!("Comment: {}", post.summary());
        if parsed.verbose && !review.trim().is_empty() {
            println!();
            println!("{review}");
        }
    }

    if !agent.success {
        return Err(format!(
            "review agent failed for run `{}`. See `{}`",
            run_id,
            run_dir.join("agent.md").display()
        ));
    }
    if post.failed() {
        return Err(format!(
            "posting the review failed for run `{}`. See `{}`",
            run_id,
            run_dir.join("write-actions.md").display()
        ));
    }

    Ok(())
}

fn command_defaults(args: Vec<String>) -> Result<(), String> {
    let defaults_args = DefaultsArgs::parse(args)?;
    let repo = resolve_repo(&defaults_args.repo)?;
    let scope = defaults_args.scope;
    let action = defaults_args.action;
    let path = defaults_path(&repo, scope)?;
    let mut store = DefaultsStore::load(&path)?;

    match action {
        DefaultsAction::Get(key) => {
            if let Some(key) = key {
                match store.values.get(&key) {
                    Some(value) => println!("{value}"),
                    None => return Err(format!("default `{key}` is not set in {scope}")),
                }
            } else if store.values.is_empty() {
                println!("No {scope} defaults set.");
            } else {
                for (key, value) in &store.values {
                    println!("{key}={value}");
                }
            }
        }
        DefaultsAction::Set(key, value) => {
            store.values.insert(key.clone(), value.clone());
            store.save(&path)?;
            println!("Set {scope} default {key}={value}");
        }
        DefaultsAction::Unset(key) => {
            store.values.remove(&key);
            store.save(&path)?;
            println!("Unset {scope} default {key}");
        }
        DefaultsAction::Export => {
            print!("{}", store.to_yaml(scope));
            io::stdout().flush().map_err(|error| error.to_string())?;
        }
    }

    Ok(())
}

fn command_show_artifact(args: Vec<String>, artifact: &str) -> Result<(), String> {
    let Some(run_id) = args.first() else {
        return Err(format!(
            "usage: vk {} <run-id|latest>",
            artifact_command_name(artifact)
        ));
    };
    let repo = current_repo()?;
    let run_dir = resolve_run_dir(&repo, run_id)?;
    let content = fs::read_to_string(run_dir.join(artifact))
        .map_err(|error| format!("cannot read `{}` for run `{}`: {error}", artifact, run_id))?;
    print!("{content}");
    Ok(())
}

fn command_doctor() -> Result<(), String> {
    println!("Valkyrie doctor");
    println!(
        "git: {}",
        if command_exists("git") {
            "ok"
        } else {
            "missing"
        }
    );
    println!("repo: {}", current_repo()?.display());
    println!(
        "repo defaults: {}",
        current_repo()?.join(".valkyrie/defaults.env").display()
    );
    println!("user defaults: {}", user_defaults_path()?.display());
    println!(
        "agent command: {}",
        detect_agent_command()
            .map(|command| format!("{} ({})", command.value, command.source))
            .unwrap_or_else(|| "missing".to_string())
    );
    Ok(())
}

#[derive(Debug)]
struct ParsedRun {
    target: Target,
    repo: PathBuf,
    dry_run: bool,
    no_write: bool,
    write: bool,
    commit: bool,
    push: bool,
    open_pr: bool,
    post_comment: bool,
    json: bool,
    verbose: bool,
    skip_validation: bool,
    validations: Vec<String>,
}

impl ParsedRun {
    fn parse(args: Vec<String>, plan_only: bool) -> Result<Self, String> {
        let mut task_parts = Vec::new();
        let mut repo = PathBuf::from(".");
        let mut dry_run = false;
        let mut no_write = plan_only;
        let mut write = false;
        let mut commit = false;
        let mut push = false;
        let mut open_pr = false;
        let mut post_comment = false;
        let mut json = false;
        let mut verbose = false;
        let mut skip_validation = false;
        let mut validations = Vec::new();

        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--repo" => repo = PathBuf::from(next_value(&mut iter, "--repo")?),
                "--validate" => validations.push(next_value(&mut iter, "--validate")?),
                "--dry-run" => dry_run = true,
                "--no-write" => no_write = true,
                "--write" => write = true,
                "--commit" => commit = true,
                "--push" => push = true,
                "--open-pr" => open_pr = true,
                "--post-comment" => post_comment = true,
                "--json" => json = true,
                "--verbose" => verbose = true,
                "--skip-validation" => skip_validation = true,
                flag if flag.starts_with('-') => return Err(format!("unknown flag `{flag}`")),
                value => task_parts.push(value.to_string()),
            }
        }

        if task_parts.is_empty() {
            return Err("usage: vk issue <number> | vk pr <number> [--repo <path>]".to_string());
        }

        Ok(Self {
            target: Target::from_parts(task_parts)?,
            repo,
            dry_run,
            no_write,
            write,
            commit,
            push,
            open_pr,
            post_comment,
            json,
            verbose,
            skip_validation,
            validations,
        })
    }
}

#[derive(Debug)]
enum Target {
    Issue(String),
    PullRequest(String),
}

impl Target {
    fn from_parts(parts: Vec<String>) -> Result<Self, String> {
        if parts.len() >= 2 && parts[0] == "issue" {
            Ok(Self::Issue(parts[1].clone()))
        } else if parts.len() >= 2 && parts[0] == "pr" {
            Ok(Self::PullRequest(parts[1].clone()))
        } else {
            Err("usage: vk issue <number> | vk pr <number> [--repo <path>]".to_string())
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Issue(_) => "github-issue",
            Self::PullRequest(_) => "github-pr",
        }
    }

    fn slug(&self) -> String {
        match self {
            Self::Issue(number) => format!("issue-{number}"),
            Self::PullRequest(number) => format!("pr-{number}"),
        }
    }
}

/// The recommendation an agent-produced PR review can carry.
///
/// `Comment` is the safe default: it leaves feedback without approving or
/// blocking the pull request. The stronger decisions map onto the
/// corresponding `gh pr review` events and only take effect when the user
/// explicitly opts into posting the review.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReviewDecision {
    Comment,
    Approve,
    RequestChanges,
}

impl ReviewDecision {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Approve => "approve",
            Self::RequestChanges => "request-changes",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "comment" => Ok(Self::Comment),
            "approve" => Ok(Self::Approve),
            "request-changes" | "request_changes" | "changes" => Ok(Self::RequestChanges),
            other => Err(format!(
                "unknown review decision `{other}`; expected one of comment, approve, request-changes"
            )),
        }
    }

    /// The `gh pr review` flag that submits this decision.
    fn gh_flag(&self) -> &'static str {
        match self {
            Self::Comment => "--comment",
            Self::Approve => "--approve",
            Self::RequestChanges => "--request-changes",
        }
    }
}

#[derive(Debug)]
struct ParsedReview {
    number: String,
    repo: PathBuf,
    plan_only: bool,
    post_comment: bool,
    decision: ReviewDecision,
    json: bool,
    verbose: bool,
}

impl ParsedReview {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut number: Option<String> = None;
        let mut repo = PathBuf::from(".");
        let mut plan_only = false;
        let mut post_comment = false;
        let mut decision = ReviewDecision::Comment;
        let mut json = false;
        let mut verbose = false;

        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--repo" => repo = PathBuf::from(next_value(&mut iter, "--repo")?),
                "--decision" => {
                    decision = ReviewDecision::parse(&next_value(&mut iter, "--decision")?)?
                }
                "--approve" => decision = ReviewDecision::Approve,
                "--request-changes" => decision = ReviewDecision::RequestChanges,
                "--plan" => plan_only = true,
                "--post-comment" | "--post-review" => post_comment = true,
                "--json" => json = true,
                "--verbose" => verbose = true,
                flag if flag.starts_with('-') => return Err(format!("unknown flag `{flag}`")),
                value if number.is_none() => number = Some(value.to_string()),
                value => return Err(format!("unexpected argument `{value}`")),
            }
        }

        let number = number
            .ok_or_else(|| "usage: vk review <number> [--repo <path>] [--plan]".to_string())?;

        Ok(Self {
            number,
            repo,
            plan_only,
            post_comment,
            decision,
            json,
            verbose,
        })
    }
}

fn render_review_plan(parsed: &ParsedReview, target_context: &TargetContext) -> String {
    let post = if parsed.post_comment {
        format!(
            "Submit a `{}` review to GitHub after analysis.",
            parsed.decision.as_str()
        )
    } else {
        "Keep the review local; remote submission is disabled.".to_string()
    };

    format!(
        "# Valkyrie PR Review Plan\n\n## Target\n\n{}\n\n## Summary\n\n{}\n\n## Proposed execution\n\n- Fetch PR metadata and diff with the GitHub CLI.\n- Ask the ACP agent to analyze the diff without modifying the working tree.\n- Capture the review as `review.md` in the run directory.\n\n## Decision\n\n- Recommendation: `{}`\n- {}\n\n## Stop conditions\n\n- Never modify the working tree during a review.\n- Do not submit a remote review unless `--post-comment` is present.\n",
        target_context.problem_statement,
        target_context.summary,
        parsed.decision.as_str(),
        post
    )
}

fn render_review_prompt(
    parsed: &ParsedReview,
    target_context: &TargetContext,
    diff: Option<&str>,
    review_path: &str,
) -> String {
    let diff_section = match diff {
        Some(diff) if !diff.trim().is_empty() => {
            format!("```diff\n{}\n```", diff.trim_end())
        }
        _ => "The diff could not be fetched automatically. Use the available tools (for example `gh pr diff`) to inspect the changes before reviewing.".to_string(),
    };

    format!(
        "# Valkyrie PR Review Task\n\nYou are reviewing GitHub pull request #{number}. This is a read-only review: do NOT modify the working tree, do not commit, and do not run write commands. Analyze the change and produce a clear, actionable code review.\n\n## Target\n\n{problem}\n\n## Context\n\n{summary}\n\n## Pull request diff\n\n{diff}\n\n## Required output\n\nWrite your review to `{review_path}` using this Markdown structure:\n\n1. `# PR #{number} Review`\n2. `## Summary` — a short description of what the PR does.\n3. `## Findings` — a list of issues, each marked `[blocker]`, `[major]`, `[minor]`, or `[nit]`, referencing files and lines where possible.\n4. `## Tests` — comments about test coverage.\n5. `## Recommendation` — one of `approve`, `request changes`, or `comment`, with a one-line justification.\n\nKeep the review focused, specific, and actionable. The maintainer requested an initial recommendation of `{decision}`, but base your final recommendation on the actual diff.\n",
        number = parsed.number,
        problem = target_context.problem_statement,
        summary = target_context.summary,
        diff = diff_section,
        review_path = review_path,
        decision = parsed.decision.as_str(),
    )
}

fn post_pr_review(
    repo: &Path,
    parsed: &ParsedReview,
    review_path: &Path,
) -> Result<WriteActionOutcome, String> {
    let body_file = review_path.display().to_string();
    let args = gh_pr_review_args(&parsed.number, parsed.decision, &body_file);
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = run_command(repo, "gh", &arg_refs)?;
    if output.success {
        Ok(WriteActionOutcome::success(
            format!(
                "submitted `{}` review on PR #{}",
                parsed.decision.as_str(),
                parsed.number
            ),
            output.stdout,
            output.stderr,
        ))
    } else {
        Ok(WriteActionOutcome::failure(
            format!("gh pr review failed for PR #{}", parsed.number),
            output.stdout,
            output.stderr,
        ))
    }
}

/// Build the `gh pr review` argument list for the requested decision.
///
/// Extracted as a pure function so the argument shape can be unit-tested
/// without shelling out to the GitHub CLI.
fn gh_pr_review_args(number: &str, decision: ReviewDecision, body_file: &str) -> Vec<String> {
    vec![
        "pr".to_string(),
        "review".to_string(),
        number.to_string(),
        decision.gh_flag().to_string(),
        "--body-file".to_string(),
        body_file.to_string(),
    ]
}

/// Map the agent outcome and review-posting outcome onto a result state.
///
/// Pure helper so the state machine can be unit-tested without running an
/// agent or the GitHub CLI.
fn review_state(agent_success: bool, post: &WriteActionOutcome) -> &'static str {
    if !agent_success || post.failed() {
        "failed"
    } else if post.success {
        "commented"
    } else {
        "reviewed"
    }
}

#[derive(Clone, Copy)]
enum DefaultsScope {
    Repo,
    User,
}

impl std::fmt::Display for DefaultsScope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Repo => write!(formatter, "repo"),
            Self::User => write!(formatter, "user"),
        }
    }
}

enum DefaultsAction {
    Get(Option<String>),
    Set(String, String),
    Unset(String),
    Export,
}

struct DefaultsArgs {
    scope: DefaultsScope,
    repo: PathBuf,
    action: DefaultsAction,
}

impl DefaultsArgs {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        if args.is_empty() {
            return Err(
                "usage: vk defaults [--repo <path>] [--global] <get|set|unset|export>".to_string(),
            );
        }

        let mut scope = DefaultsScope::Repo;
        let mut repo = PathBuf::from(".");
        let mut positional = Vec::new();
        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--global" | "--user" => scope = DefaultsScope::User,
                "--repo" => repo = PathBuf::from(next_value(&mut iter, "--repo")?),
                flag if flag.starts_with('-') => {
                    return Err(format!("unknown defaults flag `{flag}`"));
                }
                value => positional.push(value.to_string()),
            }
        }

        if positional.is_empty() {
            return Err("usage: vk defaults <get|set|unset|export> [key] [value]".to_string());
        }

        let action = match positional.remove(0).as_str() {
            "get" => DefaultsAction::Get(positional.first().cloned()),
            "set" => {
                if positional.len() < 2 {
                    return Err("usage: vk defaults set <key> <value>".to_string());
                }
                let key = positional.remove(0);
                DefaultsAction::Set(key, positional.join(" "))
            }
            "unset" => {
                let Some(key) = positional.first() else {
                    return Err("usage: vk defaults unset <key>".to_string());
                };
                DefaultsAction::Unset(key.clone())
            }
            "export" => DefaultsAction::Export,
            other => return Err(format!("unknown defaults action `{other}`")),
        };

        Ok(Self {
            scope,
            repo,
            action,
        })
    }
}

#[derive(Default)]
struct DefaultsStore {
    values: BTreeMap<String, String>,
}

impl DefaultsStore {
    fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(path)
            .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
        let mut values = BTreeMap::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                values.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
        Ok(Self { values })
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)
                .map_err(|error| format!("cannot create `{}`: {error}", dir.display()))?;
        }
        let mut content = String::from(
            "# Generated by `vk defaults set`. Prefer CLI commands over hand editing.\n",
        );
        for (key, value) in &self.values {
            content.push_str(key);
            content.push('=');
            content.push_str(value);
            content.push('\n');
        }
        write_text(path.to_path_buf(), &content)
    }

    fn to_yaml(&self, scope: DefaultsScope) -> String {
        let mut yaml = format!(
            "# Generated by `vk defaults export --{}`.\n# Prefer `vk defaults set <key> <value>` over hand-editing.\n",
            match scope {
                DefaultsScope::Repo => "repo",
                DefaultsScope::User => "global",
            }
        );
        for (key, value) in &self.values {
            yaml.push_str(&format!("{}: {}\n", key.replace('.', ":\n  "), value));
        }
        yaml
    }
}

struct DefaultsResolver {
    repo: DefaultsStore,
    user: DefaultsStore,
}

impl DefaultsResolver {
    fn load(repo: &Path) -> Result<Self, String> {
        Ok(Self {
            repo: DefaultsStore::load(&defaults_path(repo, DefaultsScope::Repo)?)?,
            user: DefaultsStore::load(&defaults_path(repo, DefaultsScope::User)?)?,
        })
    }

    fn get(&self, key: &str) -> Option<Resolved<String>> {
        env_key_for_default(key)
            .and_then(|env_key| {
                env::var(env_key)
                    .ok()
                    .map(|value| Resolved::new(value, "env"))
            })
            .or_else(|| {
                self.repo
                    .values
                    .get(key)
                    .cloned()
                    .map(|value| Resolved::new(value, "repo default"))
            })
            .or_else(|| {
                self.user
                    .values
                    .get(key)
                    .cloned()
                    .map(|value| Resolved::new(value, "user default"))
            })
    }

    fn bool(&self, key: &str) -> Option<Resolved<bool>> {
        self.get(key).and_then(|resolved| {
            parse_bool(&resolved.value).map(|value| resolved.with_value(value))
        })
    }
}

#[derive(Clone)]
struct Resolved<T> {
    value: T,
    source: String,
}

impl<T> Resolved<T> {
    fn new(value: T, source: impl Into<String>) -> Self {
        Self {
            value,
            source: source.into(),
        }
    }

    fn with_value<U>(self, value: U) -> Resolved<U> {
        Resolved {
            value,
            source: self.source,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WriteMode {
    NoWrite,
    LocalPatch,
    Commit,
    Push,
    Pr,
}

impl WriteMode {
    fn as_str(&self) -> &'static str {
        match self {
            Self::NoWrite => "no-write",
            Self::LocalPatch => "local-patch",
            Self::Commit => "commit",
            Self::Push => "push",
            Self::Pr => "pr",
        }
    }

    fn requires_clean_worktree(&self) -> bool {
        matches!(self, Self::Commit | Self::Push | Self::Pr)
    }

    fn commits_changes(&self) -> bool {
        matches!(self, Self::Commit | Self::Push | Self::Pr)
    }

    fn pushes_changes(&self) -> bool {
        matches!(self, Self::Push | Self::Pr)
    }
}

struct EffectiveSettings {
    validation: Vec<Resolved<String>>,
    agent_command: Option<Resolved<String>>,
    skip_validation: Resolved<bool>,
    write_mode: WriteMode,
    write_mode_source: String,
    commit: Resolved<bool>,
    push: Resolved<bool>,
    open_pr: Resolved<bool>,
    post_comment: Resolved<bool>,
    verbose: bool,
}

impl EffectiveSettings {
    fn from_inputs(parsed: &ParsedRun, defaults: &DefaultsResolver) -> Self {
        let skip_validation = if parsed.skip_validation {
            Resolved::new(true, "cli")
        } else {
            defaults
                .bool("validation.skip")
                .unwrap_or_else(|| Resolved::new(false, "built-in default"))
        };

        let mut validation = Vec::new();
        if !skip_validation.value {
            for command in &parsed.validations {
                validation.push(Resolved::new(command.clone(), "cli"));
            }
            if validation.is_empty()
                && let Some(command) = defaults.get("validation.command")
            {
                validation.push(command);
            }
            if validation.is_empty() {
                validation.push(Resolved::new(infer_validation_command(), "inferred"));
            }
        }

        let commit = if parsed.commit {
            Resolved::new(true, "cli")
        } else {
            defaults
                .bool("write.commit")
                .unwrap_or_else(|| Resolved::new(false, "built-in default"))
        };
        let push = if parsed.push {
            Resolved::new(true, "cli")
        } else {
            defaults
                .bool("write.push")
                .unwrap_or_else(|| Resolved::new(false, "built-in default"))
        };
        let open_pr = if parsed.open_pr {
            Resolved::new(true, "cli")
        } else {
            defaults
                .bool("write.open_pr")
                .unwrap_or_else(|| Resolved::new(false, "built-in default"))
        };
        let post_comment = if parsed.post_comment {
            Resolved::new(true, "cli")
        } else {
            defaults
                .bool("write.post_comment")
                .unwrap_or_else(|| Resolved::new(false, "built-in default"))
        };

        let (write_mode, write_mode_source) = if parsed.no_write || parsed.dry_run {
            (WriteMode::NoWrite, "cli".to_string())
        } else if open_pr.value {
            (WriteMode::Pr, open_pr.source.clone())
        } else if push.value {
            (WriteMode::Push, push.source.clone())
        } else if commit.value {
            (WriteMode::Commit, commit.source.clone())
        } else if parsed.write {
            (WriteMode::LocalPatch, "cli".to_string())
        } else {
            (WriteMode::LocalPatch, "built-in default".to_string())
        };

        Self {
            validation,
            agent_command: defaults.get("agent.command").or_else(detect_agent_command),
            skip_validation,
            write_mode,
            write_mode_source,
            commit,
            push,
            open_pr,
            post_comment,
            verbose: parsed.verbose,
        }
    }

    fn to_json(&self) -> String {
        let validation = self
            .validation
            .iter()
            .map(|resolved| {
                format!(
                    "    {{ \"command\": \"{}\", \"source\": \"{}\" }}",
                    escape_json(&resolved.value),
                    escape_json(&resolved.source)
                )
            })
            .collect::<Vec<_>>()
            .join(",\n");
        format!(
            r#"{{
  "write_mode": {{ "value": "{}", "source": "{}" }},
  "validation": [
{}
  ],
  "agent_command": {},
  "skip_validation": {{ "value": {}, "source": "{}" }},
  "commit": {{ "value": {}, "source": "{}" }},
  "push": {{ "value": {}, "source": "{}" }},
  "open_pr": {{ "value": {}, "source": "{}" }},
  "post_comment": {{ "value": {}, "source": "{}" }},
  "verbose": {}
}}
"#,
            self.write_mode.as_str(),
            escape_json(&self.write_mode_source),
            validation,
            self.agent_command
                .as_ref()
                .map(|resolved| format!(
                    "{{ \"value\": \"{}\", \"source\": \"{}\" }}",
                    escape_json(&resolved.value),
                    escape_json(&resolved.source)
                ))
                .unwrap_or_else(|| "null".to_string()),
            self.skip_validation.value,
            escape_json(&self.skip_validation.source),
            self.commit.value,
            escape_json(&self.commit.source),
            self.push.value,
            escape_json(&self.push.source),
            self.open_pr.value,
            escape_json(&self.open_pr.source),
            self.post_comment.value,
            escape_json(&self.post_comment.source),
            self.verbose,
        )
    }

    fn render_human(&self) -> String {
        let mut output = String::from("Effective settings:\n\nValidation commands:\n");
        if self.skip_validation.value {
            output.push_str(&format!(
                "  skipped    from {}\n",
                self.skip_validation.source
            ));
        } else {
            for command in &self.validation {
                output.push_str(&format!("  {}    from {}\n", command.value, command.source));
            }
        }
        output.push_str("\nAgent command:\n");
        if let Some(command) = &self.agent_command {
            output.push_str(&format!("  {}    from {}\n", command.value, command.source));
        } else {
            output.push_str("  not configured    from built-in default\n");
        }
        output.push_str("\nWrite policy:\n");
        output.push_str(&format!(
            "  mode: {}    from {}\n",
            self.write_mode.as_str(),
            self.write_mode_source
        ));
        output.push_str(&format!(
            "  commit: {}    from {}\n",
            self.commit.value, self.commit.source
        ));
        output.push_str(&format!(
            "  push: {}    from {}\n",
            self.push.value, self.push.source
        ));
        output.push_str(&format!(
            "  open_pr: {}    from {}\n",
            self.open_pr.value, self.open_pr.source
        ));
        output.push_str(&format!(
            "  post_comment: {}    from {}",
            self.post_comment.value, self.post_comment.source
        ));
        output
    }
}

struct ValidationReport {
    skipped: bool,
    results: Vec<ValidationCommandResult>,
}

impl ValidationReport {
    fn skipped() -> Self {
        Self {
            skipped: true,
            results: Vec::new(),
        }
    }

    fn failed(&self) -> bool {
        self.results.iter().any(|result| !result.success)
    }

    fn summary(&self) -> String {
        if self.skipped {
            return "skipped".to_string();
        }
        if self.results.is_empty() {
            return "not configured".to_string();
        }
        let passed = self.results.iter().filter(|result| result.success).count();
        format!("{passed}/{} passed", self.results.len())
    }

    fn to_markdown(&self) -> String {
        if self.skipped {
            return "# Validation\n\nValidation was skipped by settings.\n".to_string();
        }

        let mut markdown = String::from("# Validation\n\n");
        if self.results.is_empty() {
            markdown.push_str("No validation commands were configured.\n");
            return markdown;
        }

        for result in &self.results {
            markdown.push_str(&format!(
                "## `{}`\n\n- Source: {}\n- Exit code: {}\n- Status: {}\n\n",
                result.command,
                result.source,
                result
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "terminated by signal".to_string()),
                if result.success { "passed" } else { "failed" }
            ));
            markdown.push_str("### stdout\n\n```text\n");
            markdown.push_str(&result.stdout);
            if !result.stdout.ends_with('\n') {
                markdown.push('\n');
            }
            markdown.push_str("```\n\n### stderr\n\n```text\n");
            markdown.push_str(&result.stderr);
            if !result.stderr.ends_with('\n') {
                markdown.push('\n');
            }
            markdown.push_str("```\n\n");
        }
        markdown
    }
}

struct ValidationCommandResult {
    command: String,
    source: String,
    success: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run_validation(repo: &Path, settings: &EffectiveSettings) -> Result<ValidationReport, String> {
    if settings.skip_validation.value {
        return Ok(ValidationReport::skipped());
    }

    let mut results = Vec::new();
    for command in &settings.validation {
        let output = Command::new("sh")
            .arg("-c")
            .arg(&command.value)
            .current_dir(repo)
            .output()
            .map_err(|error| format!("cannot run validation `{}`: {error}", command.value))?;
        results.push(ValidationCommandResult {
            command: command.value.clone(),
            source: command.source.clone(),
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    Ok(ValidationReport {
        skipped: false,
        results,
    })
}

struct WriteActionReport {
    commit: WriteActionOutcome,
    push: WriteActionOutcome,
    open_pr: WriteActionOutcome,
    post_comment: WriteActionOutcome,
}

impl WriteActionReport {
    fn skipped(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            commit: WriteActionOutcome::skipped(reason.clone()),
            push: WriteActionOutcome::skipped(reason.clone()),
            open_pr: WriteActionOutcome::skipped(reason.clone()),
            post_comment: WriteActionOutcome::skipped(reason),
        }
    }

    fn failed(&self) -> bool {
        self.commit.failed()
            || self.push.failed()
            || self.open_pr.failed()
            || self.post_comment.failed()
    }

    fn committed(&self) -> bool {
        self.commit.success
    }

    fn has_remote_effect(&self) -> bool {
        self.push.success || self.open_pr.success || self.post_comment.success
    }

    fn commit_summary(&self) -> String {
        self.commit.summary()
    }

    fn push_summary(&self) -> String {
        self.push.summary()
    }

    fn remote_summary(&self) -> String {
        if self.open_pr.success && self.post_comment.success {
            "PR opened, comment posted".to_string()
        } else if self.open_pr.success {
            self.open_pr.summary()
        } else {
            self.post_comment.summary()
        }
    }

    fn to_json(&self) -> String {
        format!(
            "{{ \"commit\": {}, \"push\": {}, \"open_pr\": {}, \"post_comment\": {} }}",
            self.commit.to_json(),
            self.push.to_json(),
            self.open_pr.to_json(),
            self.post_comment.to_json()
        )
    }

    fn to_markdown(&self) -> String {
        format!(
            "# Write Actions\n\n## Commit\n\n{}\n\n## Push\n\n{}\n\n## Open PR\n\n{}\n\n## Post comment\n\n{}\n",
            self.commit.to_markdown(),
            self.push.to_markdown(),
            self.open_pr.to_markdown(),
            self.post_comment.to_markdown()
        )
    }
}

struct WriteActionOutcome {
    requested: bool,
    success: bool,
    skipped: bool,
    details: String,
    stdout: String,
    stderr: String,
}

impl WriteActionOutcome {
    fn skipped(reason: impl Into<String>) -> Self {
        Self {
            requested: false,
            success: false,
            skipped: true,
            details: reason.into(),
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn requested_skipped(reason: impl Into<String>) -> Self {
        Self {
            requested: true,
            success: false,
            skipped: true,
            details: reason.into(),
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn success(details: impl Into<String>, stdout: String, stderr: String) -> Self {
        Self {
            requested: true,
            success: true,
            skipped: false,
            details: details.into(),
            stdout,
            stderr,
        }
    }

    fn failure(details: impl Into<String>, stdout: String, stderr: String) -> Self {
        Self {
            requested: true,
            success: false,
            skipped: false,
            details: details.into(),
            stdout,
            stderr,
        }
    }

    fn failed(&self) -> bool {
        self.requested && !self.success && !self.skipped
    }

    fn summary(&self) -> String {
        if self.success {
            self.details.clone()
        } else if self.skipped {
            format!("skipped ({})", self.details)
        } else {
            format!("failed ({})", self.details)
        }
    }

    fn to_json(&self) -> String {
        format!(
            "{{ \"requested\": {}, \"success\": {}, \"skipped\": {}, \"details\": \"{}\" }}",
            self.requested,
            self.success,
            self.skipped,
            escape_json(&self.details)
        )
    }

    fn to_markdown(&self) -> String {
        let mut markdown = format!(
            "- Requested: {}\n- Success: {}\n- Skipped: {}\n- Details: {}\n",
            self.requested, self.success, self.skipped, self.details
        );
        if !self.stdout.is_empty() {
            markdown.push_str("\n### stdout\n\n```text\n");
            markdown.push_str(&self.stdout);
            if !self.stdout.ends_with('\n') {
                markdown.push('\n');
            }
            markdown.push_str("```\n");
        }
        if !self.stderr.is_empty() {
            markdown.push_str("\n### stderr\n\n```text\n");
            markdown.push_str(&self.stderr);
            if !self.stderr.ends_with('\n') {
                markdown.push('\n');
            }
            markdown.push_str("```\n");
        }
        markdown
    }
}

fn run_write_actions(
    repo: &Path,
    run_dir: &Path,
    parsed: &ParsedRun,
    settings: &EffectiveSettings,
    target_context: &TargetContext,
    changed: bool,
    validation: &ValidationReport,
) -> Result<WriteActionReport, String> {
    let commit = if settings.write_mode.commits_changes() {
        commit_changes(repo, &parsed.target, changed)?
    } else {
        WriteActionOutcome::skipped(format!(
            "write mode `{}` does not request commits",
            settings.write_mode.as_str()
        ))
    };

    let push = if settings.write_mode.pushes_changes() && !commit.failed() {
        push_current_branch(repo)?
    } else if settings.write_mode.pushes_changes() {
        WriteActionOutcome::requested_skipped("commit failed")
    } else {
        WriteActionOutcome::skipped(format!(
            "write mode `{}` does not request push",
            settings.write_mode.as_str()
        ))
    };

    let open_pr = if settings.open_pr.value && !commit.failed() && !push.failed() {
        open_pull_request(repo, run_dir, &parsed.target, target_context, validation)?
    } else if settings.open_pr.value {
        WriteActionOutcome::requested_skipped("commit or push failed")
    } else {
        WriteActionOutcome::skipped("not requested")
    };

    let post_comment = if settings.post_comment.value {
        post_target_comment(
            repo,
            run_dir,
            &parsed.target,
            validation,
            &commit,
            &push,
            &open_pr,
        )?
    } else {
        WriteActionOutcome::skipped("not requested")
    };

    Ok(WriteActionReport {
        commit,
        push,
        open_pr,
        post_comment,
    })
}

fn commit_changes(
    repo: &Path,
    target: &Target,
    changed: bool,
) -> Result<WriteActionOutcome, String> {
    if !changed {
        return Ok(WriteActionOutcome::requested_skipped(
            "no working tree changes to commit",
        ));
    }
    let add = run_command(repo, "git", &["add", "--all"])?;
    if !add.success {
        return Ok(WriteActionOutcome::failure(
            "git add failed",
            add.stdout,
            add.stderr,
        ));
    }

    let message = commit_message(target);
    let commit = run_command(repo, "git", &["commit", "-m", &message])?;
    if commit.success {
        Ok(WriteActionOutcome::success(
            format!("created commit `{}`", current_head(repo)?),
            commit.stdout,
            commit.stderr,
        ))
    } else {
        Ok(WriteActionOutcome::failure(
            "git commit failed",
            commit.stdout,
            commit.stderr,
        ))
    }
}

fn push_current_branch(repo: &Path) -> Result<WriteActionOutcome, String> {
    let branch = current_branch(repo)?;
    let push = run_command(repo, "git", &["push", "-u", "origin", &branch])?;
    if push.success {
        Ok(WriteActionOutcome::success(
            format!("pushed branch `{branch}`"),
            push.stdout,
            push.stderr,
        ))
    } else {
        Ok(WriteActionOutcome::failure(
            format!("git push failed for branch `{branch}`"),
            push.stdout,
            push.stderr,
        ))
    }
}

fn open_pull_request(
    repo: &Path,
    run_dir: &Path,
    target: &Target,
    target_context: &TargetContext,
    validation: &ValidationReport,
) -> Result<WriteActionOutcome, String> {
    let body = render_pr_body(target, target_context, validation);
    let body_file = run_dir.join("pr-body.md");
    write_text(body_file.clone(), &body)?;
    let title = pr_title(target, target_context);
    let body_file_string = body_file.display().to_string();
    let output = run_command(
        repo,
        "gh",
        &[
            "pr",
            "create",
            "--title",
            &title,
            "--body-file",
            &body_file_string,
        ],
    )?;
    if output.success {
        let stdout = output.stdout;
        let details = stdout
            .lines()
            .last()
            .unwrap_or("opened pull request")
            .to_string();
        Ok(WriteActionOutcome::success(details, stdout, output.stderr))
    } else {
        Ok(WriteActionOutcome::failure(
            "gh pr create failed",
            output.stdout,
            output.stderr,
        ))
    }
}

fn post_target_comment(
    repo: &Path,
    run_dir: &Path,
    target: &Target,
    validation: &ValidationReport,
    commit: &WriteActionOutcome,
    push: &WriteActionOutcome,
    open_pr: &WriteActionOutcome,
) -> Result<WriteActionOutcome, String> {
    let Some((kind, number)) = target_comment_target(target) else {
        return Ok(WriteActionOutcome::requested_skipped(
            "comments are only supported for GitHub issue and PR targets",
        ));
    };
    let body = render_remote_comment(target, validation, commit, push, open_pr);
    let body_file = run_dir.join("remote-comment.md");
    write_text(body_file.clone(), &body)?;
    let body_file_string = body_file.display().to_string();
    let args = [kind, "comment", number, "--body-file", &body_file_string];
    let output = run_command(repo, "gh", &args)?;
    if output.success {
        Ok(WriteActionOutcome::success(
            format!("posted {kind} comment #{number}"),
            output.stdout,
            output.stderr,
        ))
    } else {
        Ok(WriteActionOutcome::failure(
            format!("gh {kind} comment failed for #{number}"),
            output.stdout,
            output.stderr,
        ))
    }
}

fn target_comment_target(target: &Target) -> Option<(&'static str, &str)> {
    match target {
        Target::Issue(number) => Some(("issue", number)),
        Target::PullRequest(number) => Some(("pr", number)),
    }
}

fn render_remote_comment(
    target: &Target,
    validation: &ValidationReport,
    commit: &WriteActionOutcome,
    push: &WriteActionOutcome,
    open_pr: &WriteActionOutcome,
) -> String {
    format!(
        "## Valkyrie run summary\n\nTarget: {}\n\n- Validation: {}\n- Commit: {}\n- Push: {}\n- PR: {}\n\nArtifacts were captured locally in the Valkyrie run directory.\n",
        target_label(target),
        validation.summary(),
        commit.summary(),
        push.summary(),
        open_pr.summary()
    )
}

fn render_pr_body(
    target: &Target,
    target_context: &TargetContext,
    validation: &ValidationReport,
) -> String {
    format!(
        "## Summary\n\n{}\n\n## Target\n\n{}\n\n## Validation\n\n{}\n",
        target_context.problem_statement,
        target_label(target),
        validation.summary()
    )
}

fn pr_title(target: &Target, _target_context: &TargetContext) -> String {
    match target {
        Target::Issue(number) => format!("Fix issue #{number}"),
        Target::PullRequest(number) => format!("Update PR #{number}"),
    }
}

fn commit_message(target: &Target) -> String {
    match target {
        Target::Issue(number) => format!("Fix issue #{number}"),
        Target::PullRequest(number) => format!("Update PR #{number}"),
    }
}

fn target_label(target: &Target) -> String {
    match target {
        Target::Issue(number) => format!("GitHub issue #{number}"),
        Target::PullRequest(number) => format!("GitHub PR #{number}"),
    }
}

fn ensure_clean_worktree(repo: &Path) -> Result<(), String> {
    let status = git_output(repo, &["status", "--porcelain"])?;
    if status.trim().is_empty() {
        Ok(())
    } else {
        Err(format!(
            "write mode requires a clean worktree before the agent runs; commit, stash, or reset existing changes first:\n{status}"
        ))
    }
}

fn current_branch(repo: &Path) -> Result<String, String> {
    let branch = git_output(repo, &["branch", "--show-current"])?;
    let branch = branch.trim();
    if branch.is_empty() {
        Err("cannot push because HEAD is detached".to_string())
    } else {
        Ok(branch.to_string())
    }
}

fn current_head(repo: &Path) -> Result<String, String> {
    Ok(git_output(repo, &["rev-parse", "--short", "HEAD"])?
        .trim()
        .to_string())
}

struct CommandOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

fn run_command(repo: &Path, program: &str, args: &[&str]) -> Result<CommandOutput, String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|error| format!("cannot run {program}: {error}"))?;
    Ok(CommandOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

struct AgentRunReport {
    invoked: bool,
    success: bool,
    exit_code: Option<i32>,
}

fn run_agent(
    repo: &Path,
    run_dir: &Path,
    parsed: &ParsedRun,
    settings: &EffectiveSettings,
    target_context: &TargetContext,
    plan: &str,
) -> Result<AgentRunReport, String> {
    let command = settings.agent_command.as_ref().ok_or_else(|| {
        "uvx brokk acp is required for write runs but uvx was not found. Install uv or set VALKYRIE_AGENT_COMMAND/agent.command to an ACP agent command.".to_string()
    })?;
    let prompt = render_agent_prompt(parsed, settings, target_context, plan);
    invoke_acp_agent(repo, run_dir, parsed.target.kind(), command, &prompt)
}

fn invoke_acp_agent(
    repo: &Path,
    run_dir: &Path,
    target_kind: &str,
    command: &Resolved<String>,
    prompt: &str,
) -> Result<AgentRunReport, String> {
    if command.value != "uvx brokk acp" {
        return Err("custom agent.command values are not supported yet; Valkyrie currently runs the default ACP command `uvx brokk acp`.".to_string());
    }
    write_text(run_dir.join("agent-prompt.md"), prompt)?;
    let runner = write_acp_runner(run_dir, repo, target_kind, prompt, &command.value)?;
    append_event(
        run_dir,
        &format!(
            "{{\"event\":\"agent_started\",\"command\":\"{}\",\"transport\":\"acp\"}}\n",
            escape_json(&command.value)
        ),
    )?;

    let output = Command::new("uvx")
        .args(["--from", "brokk", "python"])
        .arg(&runner)
        .current_dir(repo)
        .env("VALKYRIE_RUN_DIR", run_dir)
        .env("VALKYRIE_TARGET_KIND", target_kind)
        .output()
        .map_err(|error| format!("cannot start ACP client for `{}`: {error}", command.value))?;

    let report = AgentRunReport {
        invoked: true,
        success: output.status.success(),
        exit_code: output.status.code(),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    write_text(
        run_dir.join("agent.md"),
        &render_agent_markdown(
            &command.value,
            &command.source,
            report.exit_code,
            report.success,
            &stdout,
            &stderr,
        ),
    )?;
    append_event(
        run_dir,
        &format!(
            "{{\"event\":\"agent_finished\",\"success\":{},\"exit_code\":{}}}\n",
            report.success,
            report
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "null".to_string())
        ),
    )?;

    Ok(report)
}

fn render_acp_runner_script(
    run_dir: &Path,
    repo: &Path,
    target_kind: &str,
    prompt: &str,
    agent_command: &str,
) -> String {
    format!(
        r#"import asyncio
import json
import os
import pathlib
import shlex
import sys
from typing import Any

import acp
from acp import connect_to_agent, spawn_stdio_transport, text_block
from acp.schema import (
    ClientCapabilities,
    FileSystemCapabilities,
    Implementation,
    RequestPermissionResponse,
    ReadTextFileResponse,
    WriteTextFileResponse,
)

REPO = pathlib.Path({repo_json}).resolve()
RUN_DIR = pathlib.Path({run_dir_json}).resolve()
PROMPT = {prompt_json}
TARGET_KIND = {target_kind_json}
AGENT_COMMAND = {agent_command_json}
ACP_READ_LIMIT = 16 * 1024 * 1024
MAX_EVENT_STRING = 32_000
MAX_EVENT_LIST_ITEMS = 40
MAX_EVENT_OBJECT_ITEMS = 80
TERMINALS: dict[str, dict[str, Any]] = {{}}
NEXT_TERMINAL_ID = 0


def sanitize_for_event(value: Any, depth: int = 0) -> Any:
    if depth > 8:
        return "[truncated: maximum event depth exceeded]"
    if isinstance(value, str):
        if len(value) > MAX_EVENT_STRING:
            omitted = len(value) - MAX_EVENT_STRING
            return value[:MAX_EVENT_STRING] + f"\n[truncated by Valkyrie: {{omitted}} characters omitted from event log]"
        return value
    if isinstance(value, list):
        items = [sanitize_for_event(item, depth + 1) for item in value[:MAX_EVENT_LIST_ITEMS]]
        if len(value) > MAX_EVENT_LIST_ITEMS:
            items.append(f"[truncated by Valkyrie: {{len(value) - MAX_EVENT_LIST_ITEMS}} list items omitted]")
        return items
    if isinstance(value, dict):
        sanitized: dict[str, Any] = {{}}
        items = list(value.items())
        for key, item in items[:MAX_EVENT_OBJECT_ITEMS]:
            sanitized[str(key)] = sanitize_for_event(item, depth + 1)
        if len(items) > MAX_EVENT_OBJECT_ITEMS:
            sanitized["_valkyrie_truncated"] = f"{{len(items) - MAX_EVENT_OBJECT_ITEMS}} object entries omitted"
        return sanitized
    return value


def log_event(event: dict[str, Any]) -> None:
    event = sanitize_for_event(event)
    with (RUN_DIR / "events.jsonl").open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(event, ensure_ascii=False) + "\\n")


def inside_repo(path: pathlib.Path) -> bool:
    try:
        path.resolve().relative_to(REPO)
        return True
    except ValueError:
        return False


class ValkyrieClient:
    async def session_update(self, session_id: str, update: Any, **kwargs: Any) -> None:
        payload = update.model_dump(by_alias=True, exclude_none=True) if hasattr(update, "model_dump") else update
        log_event({{"event": "session_update", "session_id": session_id, "update": payload}})

    async def request_permission(self, options: list[Any], session_id: str, tool_call: Any, **kwargs: Any) -> RequestPermissionResponse:
        chosen = None
        for option in options:
            kind = getattr(option, "kind", "")
            if kind in ("allow_once", "allow_always"):
                chosen = option
                break
        if chosen is None:
            log_event({{"event": "permission_denied", "session_id": session_id}})
            return RequestPermissionResponse(outcome={{"outcome": "cancelled"}})
        option_id = getattr(chosen, "option_id", None) or getattr(chosen, "optionId", None)
        log_event({{"event": "permission_allowed", "session_id": session_id, "option_id": option_id}})
        return RequestPermissionResponse(outcome={{"outcome": "selected", "optionId": option_id}})

    async def read_text_file(self, path: str, session_id: str, limit: int | None = None, line: int | None = None, **kwargs: Any) -> ReadTextFileResponse:
        target = (REPO / path).resolve() if not pathlib.Path(path).is_absolute() else pathlib.Path(path).resolve()
        if not inside_repo(target):
            raise RuntimeError(f"refusing to read outside repo: {{path}}")
        text = target.read_text(encoding="utf-8")
        if line is not None:
            lines = text.splitlines(keepends=True)
            text = "".join(lines[line:])
        requested_limit = limit if limit is not None else len(text)
        effective_limit = min(requested_limit, ACP_READ_LIMIT)
        truncated = len(text) > effective_limit
        text = text[:effective_limit]
        if truncated:
            text += (
                f"\n[Valkyrie truncated this file read to {{effective_limit}} characters. "
                "Request a smaller range or use search before reading more.]\n"
            )
        return ReadTextFileResponse(content=text)

    async def write_text_file(self, content: str, path: str, session_id: str, **kwargs: Any) -> WriteTextFileResponse:
        target = (REPO / path).resolve() if not pathlib.Path(path).is_absolute() else pathlib.Path(path).resolve()
        if not inside_repo(target):
            raise RuntimeError(f"refusing to write outside repo: {{path}}")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content, encoding="utf-8")
        log_event({{"event": "file_written", "session_id": session_id, "path": str(target.relative_to(REPO))}})
        return WriteTextFileResponse()

    async def create_terminal(self, command: str, session_id: str, args: list[str] | None = None, cwd: str | None = None, env: list[Any] | None = None, output_byte_limit: int | None = None, **kwargs: Any) -> dict[str, str]:
        global NEXT_TERMINAL_ID
        workdir = (REPO / cwd).resolve() if cwd and not pathlib.Path(cwd).is_absolute() else pathlib.Path(cwd or REPO).resolve()
        if not inside_repo(workdir):
            raise RuntimeError(f"refusing to run terminal outside repo: {{cwd}}")
        merged_env = dict(os.environ)
        for item in env or []:
            name = getattr(item, "name", None)
            value = getattr(item, "value", None)
            if name is not None and value is not None:
                merged_env[name] = value
        if args:
            process = await asyncio.create_subprocess_exec(
                command,
                *args,
                cwd=str(workdir),
                env=merged_env,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.STDOUT,
            )
            rendered = " ".join([command, *args])
        else:
            process = await asyncio.create_subprocess_shell(
                command,
                cwd=str(workdir),
                env=merged_env,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.STDOUT,
            )
            rendered = command
        terminal_id = f"terminal-{{NEXT_TERMINAL_ID}}"
        NEXT_TERMINAL_ID += 1
        terminal = {{"process": process, "output": bytearray(), "limit": output_byte_limit or 200_000, "truncated": False}}
        TERMINALS[terminal_id] = terminal
        log_event({{"event": "terminal_started", "session_id": session_id, "terminal_id": terminal_id, "command": rendered}})

        async def pump() -> None:
            assert process.stdout is not None
            while True:
                chunk = await process.stdout.read(4096)
                if not chunk:
                    break
                output = terminal["output"]
                output.extend(chunk)
                limit = terminal["limit"]
                if len(output) > limit:
                    del output[:-limit]
                    terminal["truncated"] = True

        terminal["pump"] = asyncio.create_task(pump())
        return {{"terminalId": terminal_id}}

    async def terminal_output(self, session_id: str, terminal_id: str, **kwargs: Any) -> dict[str, Any]:
        terminal = TERMINALS[terminal_id]
        process = terminal["process"]
        output = bytes(terminal["output"]).decode("utf-8", errors="replace")
        exit_status = None if process.returncode is None else {{"exitCode": process.returncode}}
        return {{"output": output, "truncated": terminal["truncated"], "exitStatus": exit_status}}

    async def wait_for_terminal_exit(self, session_id: str, terminal_id: str, **kwargs: Any) -> dict[str, Any]:
        terminal = TERMINALS[terminal_id]
        process = terminal["process"]
        exit_code = await process.wait()
        pump = terminal.get("pump")
        if pump is not None:
            await pump
        log_event({{"event": "terminal_finished", "session_id": session_id, "terminal_id": terminal_id, "exit_code": exit_code}})
        return {{"exitCode": exit_code}}

    async def kill_terminal(self, session_id: str, terminal_id: str, **kwargs: Any) -> dict[str, Any]:
        terminal = TERMINALS.get(terminal_id)
        if terminal is not None:
            terminal["process"].kill()
        return {{}}

    async def release_terminal(self, session_id: str, terminal_id: str, **kwargs: Any) -> dict[str, Any]:
        TERMINALS.pop(terminal_id, None)
        return {{}}


async def main() -> int:
    RUN_DIR.mkdir(parents=True, exist_ok=True)
    log_event({{"event": "acp_client_started", "target_kind": TARGET_KIND}})
    agent_args = shlex.split(AGENT_COMMAND)
    if not agent_args:
        raise RuntimeError("agent command is empty")
    async with spawn_stdio_transport(*agent_args, cwd=str(REPO), limit=ACP_READ_LIMIT) as (reader, writer, process):
        conn = connect_to_agent(ValkyrieClient(), writer, reader)
        await conn.initialize(
            protocol_version=acp.PROTOCOL_VERSION,
            client_info=Implementation(name="valkyrie", version="0.2.0"),
            client_capabilities=ClientCapabilities(
                fs=FileSystemCapabilities(read_text_file=True, write_text_file=True),
                terminal=True,
            ),
        )
        session = await conn.new_session(cwd=str(REPO), mcp_servers=[])
        response = await conn.prompt(
            session_id=session.session_id,
            prompt=[text_block(PROMPT)],
            message_id="valkyrie-run",
        )
        log_event({{
            "event": "agent_prompt_finished",
            "session_id": session.session_id,
            "stop_reason": response.stop_reason,
        }})
        with (RUN_DIR / "acp-result.json").open("w", encoding="utf-8") as handle:
            json.dump(response.model_dump(by_alias=True, exclude_none=True), handle, indent=2)
        await conn.close()
        return 0 if response.stop_reason in ("end_turn", "max_turn_requests") else 1


if __name__ == "__main__":
    try:
        raise SystemExit(asyncio.run(main()))
    except Exception as exc:
        log_event({{"event": "acp_client_failed", "error": str(exc)}})
        print(f"ACP client failed: {{exc}}", file=sys.stderr)
        raise
"#,
        repo_json = json_string_literal(&repo.display().to_string()),
        run_dir_json = json_string_literal(&run_dir.display().to_string()),
        prompt_json = json_string_literal(prompt),
        target_kind_json = json_string_literal(target_kind),
        agent_command_json = json_string_literal(agent_command),
    )
}

fn write_acp_runner(
    run_dir: &Path,
    repo: &Path,
    target_kind: &str,
    prompt: &str,
    agent_command: &str,
) -> Result<PathBuf, String> {
    let script = render_acp_runner_script(run_dir, repo, target_kind, prompt, agent_command);
    let runner = run_dir.join("acp-runner.py");
    write_text(runner.clone(), &script)?;
    Ok(runner)
}

fn render_agent_prompt(
    parsed: &ParsedRun,
    settings: &EffectiveSettings,
    target_context: &TargetContext,
    plan: &str,
) -> String {
    let validations = if settings.skip_validation.value {
        "Validation is disabled for this run.".to_string()
    } else {
        settings
            .validation
            .iter()
            .map(|command| format!("- `{}` ({})", command.value, command.source))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let target = match &parsed.target {
        Target::Issue(number) => format!("GitHub issue #{number}"),
        Target::PullRequest(number) => format!("GitHub pull request #{number}"),
    };

    format!(
        "# Valkyrie Agent Task\n\nYou are running inside the repository. Modify the working tree to complete the requested task. Keep changes focused and safe. Valkyrie will handle any requested commit, push, PR creation, or remote comments after validation; do not perform remote writes yourself.\n\n## Target\n\n{}\n\n## Problem statement\n\n{}\n\n## Context\n\n{}\n\n## Write policy\n\n- Mode: `{}` ({})\n\n## Validation commands Valkyrie will run after you finish\n\n{}\n\n## Valkyrie plan\n\n{}\n",
        target,
        target_context.problem_statement,
        target_context.summary,
        settings.write_mode.as_str(),
        settings.write_mode_source,
        validations,
        plan
    )
}

fn render_agent_markdown(
    command: &str,
    source: &str,
    exit_code: Option<i32>,
    success: bool,
    stdout: &str,
    stderr: &str,
) -> String {
    format!(
        "# Agent\n\n- Command: `{}`\n- Source: {}\n- Exit code: {}\n- Status: {}\n\n## stdout\n\n```text\n{}{}\n```\n\n## stderr\n\n```text\n{}{}\n```\n",
        command,
        source,
        exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "terminated by signal".to_string()),
        if success { "passed" } else { "failed" },
        stdout,
        if stdout.ends_with('\n') { "" } else { "\n" },
        stderr,
        if stderr.ends_with('\n') { "" } else { "\n" }
    )
}

fn append_event(run_dir: &Path, event: &str) -> Result<(), String> {
    let path = run_dir.join("events.jsonl");
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .map_err(|error| format!("cannot open `{}`: {error}", path.display()))?;
    file.write_all(event.as_bytes())
        .map_err(|error| format!("cannot write `{}`: {error}", path.display()))
}

struct TargetContext {
    problem_statement: String,
    summary: String,
    metadata_json: Option<String>,
    warning: Option<String>,
}

impl TargetContext {
    fn to_markdown(&self) -> String {
        let mut markdown = format!(
            "# Target Context\n\n## Problem statement\n\n{}\n\n## Summary\n\n{}\n",
            self.problem_statement, self.summary
        );
        if let Some(warning) = &self.warning {
            markdown.push_str("\n## Warning\n\n");
            markdown.push_str(warning);
            markdown.push('\n');
        }
        if self.metadata_json.is_some() {
            markdown.push_str("\nRaw target metadata was captured in the run directory.\n");
        }
        markdown
    }
}

fn resolve_target_context(target: &Target, repo: &Path) -> TargetContext {
    match target {
        Target::Issue(number) => resolve_issue_context(number, repo),
        Target::PullRequest(number) => resolve_pr_context(number, repo),
    }
}

fn resolve_issue_context(number: &str, repo: &Path) -> TargetContext {
    match gh_issue_view(number, repo) {
        Ok(json) => {
            let title =
                json_string_field(&json, "title").unwrap_or_else(|| format!("Issue #{number}"));
            let url = json_string_field(&json, "url");
            let state = json_string_field(&json, "state");
            let mut summary = format!("GitHub issue #{number}: {title}");
            if let Some(state) = state {
                summary.push_str(&format!("\n- State: {state}"));
            }
            if let Some(url) = url {
                summary.push_str(&format!("\n- URL: {url}"));
            }
            summary.push_str("\n- Metadata source: `gh issue view`.");

            TargetContext {
                problem_statement: format!("Fix GitHub issue #{number}: {title}"),
                summary,
                metadata_json: Some(json),
                warning: None,
            }
        }
        Err(error) => TargetContext {
            problem_statement: format!("Fix GitHub issue #{number}"),
            summary: format!(
                "GitHub issue #{number} was selected, but metadata could not be fetched."
            ),
            metadata_json: None,
            warning: Some(format!(
                "Install/authenticate the GitHub CLI (`gh`) or run in a GitHub-aware environment to fetch issue details. Fetch error: {error}"
            )),
        },
    }
}

fn resolve_pr_context(number: &str, repo: &Path) -> TargetContext {
    match gh_pr_view(number, repo) {
        Ok(json) => {
            let title =
                json_string_field(&json, "title").unwrap_or_else(|| format!("PR #{number}"));
            let url = json_string_field(&json, "url");
            let state = json_string_field(&json, "state");
            let mut summary = format!("GitHub PR #{number}: {title}");
            if let Some(state) = state {
                summary.push_str(&format!("\n- State: {state}"));
            }
            if let Some(url) = url {
                summary.push_str(&format!("\n- URL: {url}"));
            }
            summary.push_str("\n- Metadata source: `gh pr view`.");

            TargetContext {
                problem_statement: format!("Fix GitHub PR #{number}: {title}"),
                summary,
                metadata_json: Some(json),
                warning: None,
            }
        }
        Err(error) => TargetContext {
            problem_statement: format!("Fix GitHub PR #{number}"),
            summary: format!(
                "GitHub PR #{number} was selected, but metadata could not be fetched."
            ),
            metadata_json: None,
            warning: Some(format!(
                "Install/authenticate the GitHub CLI (`gh`) or run in a GitHub-aware environment to fetch PR details. Fetch error: {error}"
            )),
        },
    }
}

fn gh_issue_view(number: &str, repo: &Path) -> Result<String, String> {
    let output = Command::new("gh")
        .args([
            "issue",
            "view",
            number,
            "--json",
            "title,body,labels,comments,url,state,author",
        ])
        .current_dir(repo)
        .output()
        .map_err(|error| format!("cannot run gh: {error}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

const GH_PR_VIEW_JSON_FIELDS: &str =
    "title,body,comments,reviews,url,state,author,headRefName,baseRefName";

fn gh_pr_view(number: &str, repo: &Path) -> Result<String, String> {
    let output = Command::new("gh")
        .args(["pr", "view", number, "--json", GH_PR_VIEW_JSON_FIELDS])
        .current_dir(repo)
        .output()
        .map_err(|error| format!("cannot run gh: {error}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn gh_pr_diff(number: &str, repo: &Path) -> Result<String, String> {
    let output = Command::new("gh")
        .args(["pr", "diff", number])
        .current_dir(repo)
        .output()
        .map_err(|error| format!("cannot run gh: {error}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn json_string_field(json: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":\"");
    let start = json.find(&needle)? + needle.len();
    let mut escaped = false;
    let mut value = String::new();
    for char in json[start..].chars() {
        if escaped {
            value.push(match char {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '\\' => '\\',
                '"' => '"',
                other => other,
            });
            escaped = false;
        } else if char == '\\' {
            escaped = true;
        } else if char == '"' {
            return Some(value);
        } else {
            value.push(char);
        }
    }
    None
}

fn render_plan(
    _parsed: &ParsedRun,
    settings: &EffectiveSettings,
    target_context: &TargetContext,
    plan_only: bool,
) -> String {
    let validation = settings
        .validation
        .iter()
        .map(|resolved| format!("- `{}` ({})", resolved.value, resolved.source))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "# Valkyrie Plan\n\n## Problem statement\n\n{}\n\n## Target context\n\n{}\n\n## Proposed execution\n\n- Resolve local repository context.\n- Record effective settings and run artifacts.\n- Invoke the ACP agent through `uvx brokk acp`.\n\n## Validation\n\n{}\n\n## Write mode\n\n- `{}` ({}){}\n\n## Stop conditions\n\n- Stop before remote writes unless explicit flags are present.\n- Stop if validation fails repeatedly.\n- Stop if file-change limits are exceeded.\n",
        target_context.problem_statement,
        target_context.summary,
        validation,
        settings.write_mode.as_str(),
        settings.write_mode_source,
        if plan_only { " (planning only)" } else { "" }
    )
}

fn render_target_json(target: &Target, repo: &Path) -> String {
    match target {
        Target::Issue(number) => format!(
            "{{\n  \"kind\": \"{}\",\n  \"number\": \"{}\",\n  \"repo\": \"{}\"\n}}\n",
            target.kind(),
            escape_json(number),
            escape_json(&repo.display().to_string())
        ),
        Target::PullRequest(number) => format!(
            "{{\n  \"kind\": \"{}\",\n  \"number\": \"{}\",\n  \"repo\": \"{}\"\n}}\n",
            target.kind(),
            escape_json(number),
            escape_json(&repo.display().to_string())
        ),
    }
}

fn render_created_json(run_id: &str, run_dir: &Path, plan_only: bool) -> String {
    format!(
        "{{\n  \"run_id\": \"{}\",\n  \"run_dir\": \"{}\",\n  \"state\": \"{}\"\n}}",
        escape_json(run_id),
        escape_json(&run_dir.display().to_string()),
        if plan_only { "planned" } else { "created" }
    )
}

fn next_value(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    iter.next()
        .ok_or_else(|| format!("missing value after `{flag}`"))
}

fn make_run_id(slug: &str) -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if slug.is_empty() {
        format!("{seconds}-run")
    } else {
        format!("{seconds}-{slug}")
    }
}

fn current_repo() -> Result<PathBuf, String> {
    env::current_dir().map_err(|error| format!("cannot read current directory: {error}"))
}

fn resolve_repo(path: &Path) -> Result<PathBuf, String> {
    let repo = path
        .canonicalize()
        .map_err(|error| format!("cannot resolve repo path `{}`: {error}", path.display()))?;

    if !repo.join(".git").exists() {
        return Err(format!(
            "`{}` does not look like a git repository",
            repo.display()
        ));
    }

    Ok(repo)
}

fn defaults_path(repo: &Path, scope: DefaultsScope) -> Result<PathBuf, String> {
    match scope {
        DefaultsScope::Repo => Ok(repo.join(".valkyrie").join("defaults.env")),
        DefaultsScope::User => Ok(user_defaults_path()?),
    }
}

fn user_defaults_path() -> Result<PathBuf, String> {
    if let Ok(path) = env::var("VALKYRIE_DEFAULTS_PATH") {
        return Ok(PathBuf::from(path));
    }
    let home = env::var("HOME").map_err(|_| {
        "cannot resolve user defaults path: HOME is not set and VALKYRIE_DEFAULTS_PATH was not provided"
            .to_string()
    })?;
    Ok(PathBuf::from(home).join(".config/valkyrie/defaults.env"))
}

fn resolve_run_dir(repo: &Path, run_id: &str) -> Result<PathBuf, String> {
    let runs = repo.join(".valkyrie").join("runs");
    if run_id == "latest" {
        let mut entries = fs::read_dir(&runs)
            .map_err(|error| format!("cannot read runs directory `{}`: {error}", runs.display()))?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        return entries
            .last()
            .map(|entry| entry.path())
            .ok_or_else(|| "no runs found".to_string());
    }
    Ok(runs.join(run_id))
}

fn artifact_command_name(artifact: &str) -> &str {
    match artifact {
        "result.json" => "status",
        "events.jsonl" => "logs",
        "diff.patch" => "diff",
        _ => "show",
    }
}

fn git_output(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

fn command_exists(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn write_text(path: PathBuf, content: &str) -> Result<(), String> {
    fs::write(&path, content).map_err(|error| format!("cannot write `{}`: {error}", path.display()))
}

fn escape_json(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn json_string_literal(value: &str) -> String {
    format!("\"{}\"", escape_json(value))
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn env_key_for_default(key: &str) -> Option<&'static str> {
    match key {
        "validation.command" => Some("VALKYRIE_VALIDATION_COMMAND"),
        "validation.skip" => Some("VALKYRIE_SKIP_VALIDATION"),
        "agent.command" => Some("VALKYRIE_AGENT_COMMAND"),
        "write.commit" => Some("VALKYRIE_WRITE_COMMIT"),
        "write.push" => Some("VALKYRIE_WRITE_PUSH"),
        "write.open_pr" => Some("VALKYRIE_WRITE_OPEN_PR"),
        "write.post_comment" => Some("VALKYRIE_WRITE_POST_COMMENT"),
        _ => None,
    }
}

fn detect_agent_command() -> Option<Resolved<String>> {
    if command_exists("uvx") {
        Some(Resolved::new(
            "uvx brokk acp".to_string(),
            "detected uvx brokk acp",
        ))
    } else {
        None
    }
}

fn infer_validation_command() -> String {
    if Path::new("Cargo.toml").exists() {
        "cargo test".to_string()
    } else if Path::new("package.json").exists() {
        "npm test".to_string()
    } else {
        "echo 'no validation command inferred'".to_string()
    }
}

fn print_help() {
    println!(
        "Valkyrie automation CLI\n\nUsage:\n  valkyrie issue <number> [--repo <path>] [--plan] [--validate <command>] [--no-write|--write] [--commit] [--push] [--open-pr] [--post-comment] [--skip-validation] [--json] [--verbose]\n  valkyrie pr <number> [--repo <path>] [--fix] [--plan] [--validate <command>] [--no-write|--write] [--commit] [--push] [--open-pr] [--post-comment] [--skip-validation] [--json] [--verbose]\n  valkyrie review <number> [--repo <path>] [--plan] [--decision <comment|approve|request-changes>] [--post-comment]\n  vk defaults [--repo <path>] [--global] get [key]\n  vk defaults [--repo <path>] [--global] set <key> <value>\n  vk defaults [--repo <path>] [--global] unset <key>\n  vk defaults [--repo <path>] [--global] export\n  valkyrie status <run-id|latest>\n  valkyrie logs <run-id|latest>\n  valkyrie diff <run-id|latest>\n  valkyrie doctor\n\nDefaults precedence for runs: CLI flags > environment variables > repo defaults > user defaults > built-in defaults. Remote writes stay disabled unless explicitly requested."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_parses_issue_alias() {
        let target = Target::from_parts(vec!["issue".to_string(), "123".to_string()]).unwrap();
        assert_eq!(target.kind(), "github-issue");
        assert_eq!(target.slug(), "issue-123");
    }

    #[test]
    fn target_parses_pull_request_alias() {
        let target = Target::from_parts(vec!["pr".to_string(), "456".to_string()]).unwrap();
        assert_eq!(target.kind(), "github-pr");
        assert_eq!(target.slug(), "pr-456");
    }

    #[test]
    fn target_from_parts_rejects_unknown_aliases() {
        assert!(Target::from_parts(vec!["fix".to_string(), "parser".to_string()]).is_err());
        assert!(Target::from_parts(vec!["issue".to_string()]).is_err());
    }

    #[test]
    fn render_target_json_includes_pull_request_kind_and_number() {
        let target = Target::PullRequest("456".to_string());
        let json = render_target_json(&target, Path::new("/tmp/example"));

        assert!(json.contains("\"kind\": \"github-pr\""));
        assert!(json.contains("\"number\": \"456\""));
        assert!(json.contains("\"repo\": \"/tmp/example\""));
    }

    #[test]
    fn parsed_run_parse_captures_flags_and_pull_request_target() {
        let parsed = ParsedRun::parse(
            vec![
                "pr".to_string(),
                "456".to_string(),
                "--repo".to_string(),
                ".".to_string(),
                "--validate".to_string(),
                "cargo test".to_string(),
                "--dry-run".to_string(),
                "--commit".to_string(),
                "--push".to_string(),
                "--open-pr".to_string(),
                "--post-comment".to_string(),
                "--json".to_string(),
                "--verbose".to_string(),
            ],
            false,
        )
        .unwrap();

        assert!(matches!(parsed.target, Target::PullRequest(ref number) if number == "456"));
        assert_eq!(parsed.repo, PathBuf::from("."));
        assert_eq!(parsed.validations, vec!["cargo test".to_string()]);
        assert!(parsed.dry_run);
        assert!(parsed.commit);
        assert!(parsed.push);
        assert!(parsed.open_pr);
        assert!(parsed.post_comment);
        assert!(parsed.json);
        assert!(parsed.verbose);
    }

    #[test]
    fn parsed_run_parse_rejects_missing_target_unknown_flag_and_missing_flag_value() {
        assert!(ParsedRun::parse(Vec::new(), false).is_err());
        assert!(
            ParsedRun::parse(
                vec![
                    "issue".to_string(),
                    "1".to_string(),
                    "--unknown".to_string()
                ],
                false
            )
            .is_err()
        );
        assert!(
            ParsedRun::parse(
                vec!["issue".to_string(), "1".to_string(), "--repo".to_string()],
                false
            )
            .is_err()
        );
        assert!(ParsedRun::parse(vec!["fix".to_string(), "parser".to_string()], false).is_err());
    }

    #[test]
    fn defaults_args_parse_set_global_and_errors() {
        let parsed = DefaultsArgs::parse(vec![
            "--global".to_string(),
            "set".to_string(),
            "validation.command".to_string(),
            "cargo".to_string(),
            "test".to_string(),
        ])
        .unwrap();

        assert!(matches!(parsed.scope, DefaultsScope::User));
        match parsed.action {
            DefaultsAction::Set(key, value) => {
                assert_eq!(key, "validation.command");
                assert_eq!(value, "cargo test");
            }
            _ => panic!("expected set action"),
        }

        assert!(DefaultsArgs::parse(Vec::new()).is_err());
        assert!(DefaultsArgs::parse(vec!["set".to_string(), "only-key".to_string()]).is_err());
        assert!(DefaultsArgs::parse(vec!["unset".to_string()]).is_err());
        assert!(DefaultsArgs::parse(vec!["unknown".to_string()]).is_err());
    }

    #[test]
    fn defaults_store_loads_env_file_and_exports_yaml() {
        let path = env::temp_dir().join(format!("valkyrie-defaults-{}.env", make_run_id("test")));
        fs::write(
            &path,
            "# comment\n\nvalidation.command = cargo test\nwrite.commit=true\nignored-without-equals\n",
        )
        .unwrap();

        let store = DefaultsStore::load(&path).unwrap();
        assert_eq!(
            store.values.get("validation.command"),
            Some(&"cargo test".to_string())
        );
        assert_eq!(store.values.get("write.commit"), Some(&"true".to_string()));
        assert!(!store.values.contains_key("ignored-without-equals"));

        let yaml = store.to_yaml(DefaultsScope::Repo);
        assert!(yaml.contains("# Generated by `vk defaults export --repo`."));
        assert!(yaml.contains("validation:\n  command: cargo test"));

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn effective_settings_selects_write_modes_from_cli_flags() {
        let defaults = DefaultsResolver {
            repo: DefaultsStore::default(),
            user: DefaultsStore::default(),
        };

        let mut parsed = ParsedRun::parse(
            vec![
                "issue".to_string(),
                "123".to_string(),
                "--no-write".to_string(),
            ],
            false,
        )
        .unwrap();
        let settings = EffectiveSettings::from_inputs(&parsed, &defaults);
        assert_eq!(settings.write_mode, WriteMode::NoWrite);
        assert_eq!(settings.write_mode_source, "cli");

        parsed = ParsedRun::parse(
            vec![
                "issue".to_string(),
                "123".to_string(),
                "--commit".to_string(),
            ],
            false,
        )
        .unwrap();
        let settings = EffectiveSettings::from_inputs(&parsed, &defaults);
        assert_eq!(settings.write_mode, WriteMode::Commit);

        parsed = ParsedRun::parse(
            vec!["issue".to_string(), "123".to_string(), "--push".to_string()],
            false,
        )
        .unwrap();
        let settings = EffectiveSettings::from_inputs(&parsed, &defaults);
        assert_eq!(settings.write_mode, WriteMode::Push);

        parsed = ParsedRun::parse(
            vec![
                "issue".to_string(),
                "123".to_string(),
                "--open-pr".to_string(),
            ],
            false,
        )
        .unwrap();
        let settings = EffectiveSettings::from_inputs(&parsed, &defaults);
        assert_eq!(settings.write_mode, WriteMode::Pr);
    }

    #[test]
    fn target_context_markdown_includes_warning_and_generic_metadata_note() {
        let context = TargetContext {
            problem_statement: "Fix PR #456".to_string(),
            summary: "PR context".to_string(),
            metadata_json: Some("{}".to_string()),
            warning: Some("gh is unavailable".to_string()),
        };

        let markdown = context.to_markdown();
        assert!(markdown.contains("Fix PR #456"));
        assert!(markdown.contains("gh is unavailable"));
        assert!(markdown.contains("Raw target metadata was captured"));
        assert!(!markdown.contains("issue.json"));
    }

    #[test]
    fn gh_pr_view_fields_do_not_include_unsupported_review_threads() {
        assert!(GH_PR_VIEW_JSON_FIELDS.contains("reviews"));
        assert!(GH_PR_VIEW_JSON_FIELDS.contains("comments"));
        assert!(!GH_PR_VIEW_JSON_FIELDS.contains("reviewThreads"));
    }

    #[test]
    fn validation_report_markdown_covers_failed_command_without_newlines() {
        let report = ValidationReport {
            skipped: false,
            results: vec![ValidationCommandResult {
                command: "false".to_string(),
                source: "cli".to_string(),
                success: false,
                exit_code: None,
                stdout: "no-newline".to_string(),
                stderr: "err".to_string(),
            }],
        };

        assert!(report.failed());
        let markdown = report.to_markdown();
        assert!(markdown.contains("- Exit code: terminated by signal"));
        assert!(markdown.contains("- Status: failed"));
        assert!(markdown.contains("no-newline\n```"));
        assert!(markdown.contains("err\n```"));
    }

    #[test]
    fn render_created_json_uses_planned_or_created_state() {
        let planned = render_created_json("run-1", Path::new("/tmp/run-1"), true);
        let created = render_created_json("run-2", Path::new("/tmp/run-2"), false);

        assert!(planned.contains("\"state\": \"planned\""));
        assert!(created.contains("\"state\": \"created\""));
    }

    #[test]
    fn bool_parser_accepts_common_values() {
        assert_eq!(parse_bool("true"), Some(true));
        assert_eq!(parse_bool("off"), Some(false));
        assert_eq!(parse_bool("maybe"), None);
    }

    #[test]
    fn extracts_simple_json_string_fields() {
        let json =
            r#"{"title":"Fix parser panic","state":"OPEN","url":"https://example.test/issue/1"}"#;
        assert_eq!(
            json_string_field(json, "title"),
            Some("Fix parser panic".to_string())
        );
        assert_eq!(json_string_field(json, "state"), Some("OPEN".to_string()));
        assert_eq!(json_string_field(json, "missing"), None);
    }

    #[test]
    fn validation_report_markdown_includes_command_output() {
        let report = ValidationReport {
            skipped: false,
            results: vec![ValidationCommandResult {
                command: "cargo test".to_string(),
                source: "cli".to_string(),
                success: true,
                exit_code: Some(0),
                stdout: "ok\n".to_string(),
                stderr: String::new(),
            }],
        };

        let markdown = report.to_markdown();
        assert!(markdown.contains("## `cargo test`"));
        assert!(markdown.contains("- Source: cli"));
        assert!(markdown.contains("- Status: passed"));
        assert!(markdown.contains("ok"));
    }

    #[test]
    fn write_mode_helpers_select_commit_and_remote_actions() {
        assert!(!WriteMode::LocalPatch.requires_clean_worktree());
        assert!(!WriteMode::LocalPatch.commits_changes());
        assert!(!WriteMode::Commit.pushes_changes());
        assert!(WriteMode::Commit.requires_clean_worktree());
        assert!(WriteMode::Commit.commits_changes());
        assert!(WriteMode::Push.pushes_changes());
        assert!(WriteMode::Pr.pushes_changes());
    }

    #[test]
    fn target_comment_target_supports_only_remote_targets() {
        let issue = Target::Issue("12".to_string());
        let pr = Target::PullRequest("34".to_string());

        assert_eq!(target_comment_target(&issue), Some(("issue", "12")));
        assert_eq!(target_comment_target(&pr), Some(("pr", "34")));
    }

    #[test]
    fn render_remote_comment_summarizes_write_actions() {
        let validation = ValidationReport {
            skipped: false,
            results: vec![ValidationCommandResult {
                command: "cargo test".to_string(),
                source: "inferred".to_string(),
                success: true,
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            }],
        };
        let commit =
            WriteActionOutcome::success("created commit `abc1234`", String::new(), String::new());
        let push = WriteActionOutcome::skipped("not requested");
        let open_pr = WriteActionOutcome::skipped("not requested");

        let comment = render_remote_comment(
            &Target::PullRequest("11".to_string()),
            &validation,
            &commit,
            &push,
            &open_pr,
        );

        assert!(comment.contains("Target: GitHub PR #11"));
        assert!(comment.contains("- Validation: 1/1 passed"));
        assert!(comment.contains("- Commit: created commit `abc1234`"));
        assert!(comment.contains("- Push: skipped (not requested)"));
    }

    #[test]
    fn write_action_report_marks_failures_and_remote_effects() {
        let success =
            WriteActionOutcome::success("posted pr comment #11", String::new(), String::new());
        let failure =
            WriteActionOutcome::failure("git push failed", String::new(), "denied".to_string());
        let skipped = WriteActionOutcome::skipped("not requested");

        assert!(success.summary().contains("posted pr comment"));
        assert!(failure.failed());
        assert!(!skipped.failed());

        let report = WriteActionReport {
            commit: skipped,
            push: WriteActionOutcome::skipped("not requested"),
            open_pr: WriteActionOutcome::skipped("not requested"),
            post_comment: success,
        };
        assert!(report.has_remote_effect());
        assert!(!report.failed());
    }

    #[test]
    fn skipped_validation_report_is_not_failed() {
        let report = ValidationReport::skipped();
        assert!(!report.failed());
        assert!(report.to_markdown().contains("Validation was skipped"));
    }

    #[test]
    fn review_decision_parses_known_values_and_rejects_unknown() {
        assert_eq!(
            ReviewDecision::parse("comment").unwrap(),
            ReviewDecision::Comment
        );
        assert_eq!(
            ReviewDecision::parse("APPROVE").unwrap(),
            ReviewDecision::Approve
        );
        assert_eq!(
            ReviewDecision::parse("request-changes").unwrap(),
            ReviewDecision::RequestChanges
        );
        assert_eq!(
            ReviewDecision::parse("request_changes").unwrap(),
            ReviewDecision::RequestChanges
        );
        assert!(ReviewDecision::parse("merge").is_err());
    }

    #[test]
    fn review_decision_maps_to_gh_flags() {
        assert_eq!(ReviewDecision::Comment.gh_flag(), "--comment");
        assert_eq!(ReviewDecision::Approve.gh_flag(), "--approve");
        assert_eq!(
            ReviewDecision::RequestChanges.gh_flag(),
            "--request-changes"
        );
    }

    #[test]
    fn parsed_review_defaults_to_local_comment_review() {
        let parsed = ParsedReview::parse(vec!["456".to_string()]).unwrap();

        assert_eq!(parsed.number, "456");
        assert_eq!(parsed.repo, PathBuf::from("."));
        assert!(!parsed.plan_only);
        assert!(!parsed.post_comment);
        assert_eq!(parsed.decision, ReviewDecision::Comment);
        assert!(!parsed.json);
        assert!(!parsed.verbose);
    }

    #[test]
    fn parsed_review_captures_flags_and_decision_shortcuts() {
        let parsed = ParsedReview::parse(vec![
            "789".to_string(),
            "--repo".to_string(),
            ".".to_string(),
            "--request-changes".to_string(),
            "--post-comment".to_string(),
            "--json".to_string(),
            "--verbose".to_string(),
            "--plan".to_string(),
        ])
        .unwrap();

        assert_eq!(parsed.number, "789");
        assert_eq!(parsed.decision, ReviewDecision::RequestChanges);
        assert!(parsed.post_comment);
        assert!(parsed.json);
        assert!(parsed.verbose);
        assert!(parsed.plan_only);
    }

    #[test]
    fn parsed_review_decision_flag_overrides_with_explicit_value() {
        let parsed = ParsedReview::parse(vec![
            "12".to_string(),
            "--decision".to_string(),
            "approve".to_string(),
        ])
        .unwrap();

        assert_eq!(parsed.decision, ReviewDecision::Approve);
    }

    #[test]
    fn parsed_review_rejects_missing_number_and_extra_positional() {
        assert!(ParsedReview::parse(Vec::new()).is_err());
        assert!(
            ParsedReview::parse(vec!["1".to_string(), "2".to_string()]).is_err(),
            "a second positional argument must be rejected"
        );
        assert!(ParsedReview::parse(vec!["1".to_string(), "--unknown".to_string()]).is_err());
        assert!(
            ParsedReview::parse(vec!["1".to_string(), "--decision".to_string()]).is_err(),
            "missing decision value must be rejected"
        );
    }

    #[test]
    fn render_review_plan_reflects_post_choice() {
        let context = TargetContext {
            problem_statement: "Fix GitHub PR #456".to_string(),
            summary: "PR context".to_string(),
            metadata_json: None,
            warning: None,
        };

        let local = ParsedReview::parse(vec!["456".to_string()]).unwrap();
        let plan = render_review_plan(&local, &context);
        assert!(plan.contains("# Valkyrie PR Review Plan"));
        assert!(plan.contains("Recommendation: `comment`"));
        assert!(plan.contains("remote submission is disabled"));

        let remote = ParsedReview::parse(vec![
            "456".to_string(),
            "--approve".to_string(),
            "--post-comment".to_string(),
        ])
        .unwrap();
        let plan = render_review_plan(&remote, &context);
        assert!(plan.contains("Submit a `approve` review"));
    }

    #[test]
    fn render_review_prompt_embeds_diff_and_review_path() {
        let context = TargetContext {
            problem_statement: "Fix GitHub PR #456".to_string(),
            summary: "PR context".to_string(),
            metadata_json: None,
            warning: None,
        };
        let parsed = ParsedReview::parse(vec!["456".to_string()]).unwrap();

        let prompt = render_review_prompt(
            &parsed,
            &context,
            Some("diff --git a/foo b/foo\n+added"),
            "review.md",
        );
        assert!(prompt.contains("read-only review"));
        assert!(prompt.contains("```diff"));
        assert!(prompt.contains("diff --git a/foo b/foo"));
        assert!(prompt.contains("Write your review to `review.md`"));

        let without_diff = render_review_prompt(&parsed, &context, None, "review.md");
        assert!(without_diff.contains("diff could not be fetched automatically"));
    }

    #[test]
    fn render_review_prompt_ignores_blank_diff() {
        let context = TargetContext {
            problem_statement: "Fix GitHub PR #1".to_string(),
            summary: "PR context".to_string(),
            metadata_json: None,
            warning: None,
        };
        let parsed = ParsedReview::parse(vec!["1".to_string()]).unwrap();

        let prompt = render_review_prompt(&parsed, &context, Some("   \n  \n"), "review.md");
        assert!(!prompt.contains("```diff"));
        assert!(prompt.contains("diff could not be fetched automatically"));
    }

    #[test]
    fn render_review_prompt_uses_provided_review_path() {
        let context = TargetContext {
            problem_statement: "Fix GitHub PR #1".to_string(),
            summary: "PR context".to_string(),
            metadata_json: None,
            warning: None,
        };
        let parsed = ParsedReview::parse(vec!["1".to_string()]).unwrap();

        let prompt = render_review_prompt(
            &parsed,
            &context,
            Some("diff"),
            ".valkyrie/runs/run-1/review.md",
        );
        assert!(prompt.contains("Write your review to `.valkyrie/runs/run-1/review.md`"));
    }

    #[test]
    fn render_review_plan_reflects_request_changes_decision() {
        let context = TargetContext {
            problem_statement: "Fix GitHub PR #99".to_string(),
            summary: "PR context".to_string(),
            metadata_json: None,
            warning: None,
        };
        let parsed = ParsedReview::parse(vec![
            "99".to_string(),
            "--request-changes".to_string(),
            "--post-comment".to_string(),
        ])
        .unwrap();

        let plan = render_review_plan(&parsed, &context);
        assert!(plan.contains("Recommendation: `request-changes`"));
        assert!(plan.contains("Submit a `request-changes` review"));
    }

    #[test]
    fn gh_pr_review_args_builds_expected_command_for_each_decision() {
        assert_eq!(
            gh_pr_review_args("456", ReviewDecision::Comment, "/tmp/review.md"),
            vec![
                "pr".to_string(),
                "review".to_string(),
                "456".to_string(),
                "--comment".to_string(),
                "--body-file".to_string(),
                "/tmp/review.md".to_string(),
            ]
        );
        assert_eq!(
            gh_pr_review_args("7", ReviewDecision::Approve, "review.md")[3],
            "--approve".to_string()
        );
        assert_eq!(
            gh_pr_review_args("7", ReviewDecision::RequestChanges, "review.md")[3],
            "--request-changes".to_string()
        );
    }

    #[test]
    fn review_state_handles_agent_and_post_outcomes() {
        let posted = WriteActionOutcome::success("submitted review", String::new(), String::new());
        let not_requested = WriteActionOutcome::skipped("not requested");
        let failed_post =
            WriteActionOutcome::failure("gh pr review failed", String::new(), String::new());

        // Agent failure dominates regardless of the posting outcome.
        assert_eq!(review_state(false, &not_requested), "failed");
        assert_eq!(review_state(false, &posted), "failed");

        // Successful agent without a remote post is a local review.
        assert_eq!(review_state(true, &not_requested), "reviewed");

        // Successful agent and successful post becomes a commented review.
        assert_eq!(review_state(true, &posted), "commented");

        // A failed post turns the whole run into a failure.
        assert_eq!(review_state(true, &failed_post), "failed");
    }
}
