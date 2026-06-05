use std::collections::BTreeMap;
use std::env;
use std::fs;
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
        "run" => command_run(args, false),
        "plan" => command_plan(args),
        "issue" => command_issue(args),
        "pr" => command_pr(args),
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
            "unknown command `{other}`. Run `valkyrie help` for usage."
        )),
    }
}

fn command_run(args: Vec<String>, plan_only: bool) -> Result<(), String> {
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
            Target::LocalTask(_) => "metadata.json",
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

    let diff = git_output(&repo, &["diff", "--"])?;
    write_text(run_dir.join("diff.patch"), &diff)?;

    let validation = run_validation(&repo, &settings)?;
    write_text(run_dir.join("validation.md"), &validation.to_markdown())?;
    let state = if validation.failed() {
        "failed"
    } else if validation.skipped {
        "planned"
    } else {
        "validated"
    };

    write_text(
        run_dir.join("summary.md"),
        &format!(
            "# Summary

Run record created. Agent execution is not wired yet; this MVP skeleton records the target, settings, plan, logs, current diff, and validation result.

Validation state: `{state}`.
"
        ),
    )?;
    write_text(
        run_dir.join("result.json"),
        &format!(
            r#"{{
  "run_id": "{}",
  "state": "{}",
  "run_dir": "{}",
  "agent_invoked": false,
  "validation": {{ "skipped": {}, "failed": {} }}
}}
"#,
            escape_json(&run_id),
            state,
            escape_json(&run_dir.display().to_string()),
            validation.skipped,
            validation.failed()
        ),
    )?;

    if validation.failed() {
        return Err(format!(
            "validation failed for run `{}`. See `{}`",
            run_id,
            run_dir.join("validation.md").display()
        ));
    }

    Ok(())
}

fn command_plan(args: Vec<String>) -> Result<(), String> {
    if args.is_empty() {
        return Err("usage: valkyrie plan <task>|issue <number> [--repo <path>]".to_string());
    }

    command_run(rewrite_target_alias(args), true)
}

fn command_issue(args: Vec<String>) -> Result<(), String> {
    if args.is_empty() {
        return Err("usage: valkyrie issue <number> [--repo <path>] [--plan]".to_string());
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
    command_run(rewritten, plan_only)
}

fn command_pr(args: Vec<String>) -> Result<(), String> {
    if args.is_empty() {
        return Err("usage: valkyrie pr <number> [--repo <path>] [--fix] [--plan]".to_string());
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
    command_run(rewritten, plan_only)
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
            "usage: valkyrie {} <run-id|latest>",
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
        "anvil: {}",
        if command_exists("anvil") {
            "ok"
        } else {
            "missing"
        }
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
            return Err("usage: valkyrie run <task> [--repo <path>]".to_string());
        }

        Ok(Self {
            target: Target::from_parts(task_parts),
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
    LocalTask(String),
    Issue(String),
    PullRequest(String),
}

impl Target {
    fn from_parts(parts: Vec<String>) -> Self {
        if parts.len() >= 2 && parts[0] == "issue" {
            Self::Issue(parts[1].clone())
        } else if parts.len() >= 2 && parts[0] == "pr" {
            Self::PullRequest(parts[1].clone())
        } else {
            Self::LocalTask(parts.join(" "))
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::LocalTask(_) => "local-task",
            Self::Issue(_) => "github-issue",
            Self::PullRequest(_) => "github-pr",
        }
    }

    fn slug(&self) -> String {
        match self {
            Self::LocalTask(task) => slugify(task),
            Self::Issue(number) => format!("issue-{number}"),
            Self::PullRequest(number) => format!("pr-{number}"),
        }
    }
}

fn rewrite_target_alias(args: Vec<String>) -> Vec<String> {
    if args.len() >= 2 && (args[0] == "issue" || args[0] == "pr") {
        let mut rewritten = vec![args[0].clone(), args[1].clone()];
        rewritten.extend(args.into_iter().skip(2));
        rewritten
    } else {
        args
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
                "usage: valkyrie defaults [--repo <path>] [--global] <get|set|unset|export>"
                    .to_string(),
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
            return Err(
                "usage: valkyrie defaults <get|set|unset|export> [key] [value]".to_string(),
            );
        }

        let action = match positional.remove(0).as_str() {
            "get" => DefaultsAction::Get(positional.first().cloned()),
            "set" => {
                if positional.len() < 2 {
                    return Err("usage: valkyrie defaults set <key> <value>".to_string());
                }
                let key = positional.remove(0);
                DefaultsAction::Set(key, positional.join(" "))
            }
            "unset" => {
                let Some(key) = positional.first() else {
                    return Err("usage: valkyrie defaults unset <key>".to_string());
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
            "# Generated by `valkyrie defaults set`. Prefer CLI commands over hand editing.\n",
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
            "# Generated by `valkyrie defaults export --{}`.\n# Prefer `valkyrie defaults set <key> <value>` over hand-editing.\n",
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
}

struct EffectiveSettings {
    validation: Vec<Resolved<String>>,
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
        Target::LocalTask(task) => TargetContext {
            problem_statement: task.clone(),
            summary: "Local repository task provided directly on the CLI.".to_string(),
            metadata_json: None,
            warning: None,
        },
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
        "# Valkyrie Plan\n\n## Problem statement\n\n{}\n\n## Target context\n\n{}\n\n## Proposed execution\n\n- Resolve local repository context.\n- Record effective settings and run artifacts.\n- Prepare for agent execution through anvil and code intelligence through bifrost.\n\n## Validation\n\n{}\n\n## Write mode\n\n- `{}` ({}){}\n\n## Stop conditions\n\n- Stop before remote writes unless explicit flags are present.\n- Stop if validation fails repeatedly.\n- Stop if file-change limits are exceeded.\n",
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
        Target::LocalTask(task) => format!(
            "{{\n  \"kind\": \"{}\",\n  \"task\": \"{}\",\n  \"repo\": \"{}\"\n}}\n",
            target.kind(),
            escape_json(task),
            escape_json(&repo.display().to_string())
        ),
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

fn slugify(input: &str) -> String {
    input
        .chars()
        .map(|char| {
            if char.is_ascii_alphanumeric() {
                char.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(40)
        .collect()
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
        "write.commit" => Some("VALKYRIE_WRITE_COMMIT"),
        "write.push" => Some("VALKYRIE_WRITE_PUSH"),
        "write.open_pr" => Some("VALKYRIE_WRITE_OPEN_PR"),
        "write.post_comment" => Some("VALKYRIE_WRITE_POST_COMMENT"),
        _ => None,
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
        "Valkyrie automation CLI\n\nUsage:\n  valkyrie issue <number> [--repo <path>] [--plan]\n  valkyrie pr <number> [--repo <path>] [--fix] [--plan]\n  valkyrie run <task> [--repo <path>] [--validate <command>] [--no-write|--write] [--skip-validation] [--json] [--verbose]\n  valkyrie plan <task>|issue <number>|pr <number> [--repo <path>]\n  valkyrie defaults [--repo <path>] [--global] get [key]\n  valkyrie defaults [--repo <path>] [--global] set <key> <value>\n  valkyrie defaults [--repo <path>] [--global] unset <key>\n  valkyrie defaults [--repo <path>] [--global] export\n  valkyrie status <run-id|latest>\n  valkyrie logs <run-id|latest>\n  valkyrie diff <run-id|latest>\n  valkyrie doctor\n\nDefaults precedence for runs: CLI flags > environment variables > repo defaults > user defaults > built-in defaults. Remote writes stay disabled unless explicitly requested."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_limits_and_normalizes() {
        assert_eq!(slugify("Fix the Parser Panic!"), "fix-the-parser-panic");
        assert_eq!(slugify("---"), "");
    }

    #[test]
    fn target_parses_local_task_when_no_known_alias_is_present() {
        let target = Target::from_parts(vec!["fix".to_string(), "parser".to_string()]);
        assert_eq!(target.kind(), "local-task");
        assert_eq!(target.slug(), "fix-parser");
    }

    #[test]
    fn rewrite_target_alias_leaves_local_tasks_unchanged() {
        let args = vec![
            "fix".to_string(),
            "parser".to_string(),
            "--repo".to_string(),
            ".".to_string(),
        ];

        assert_eq!(rewrite_target_alias(args.clone()), args);
    }

    #[test]
    fn target_parses_issue_alias() {
        let target = Target::from_parts(vec!["issue".to_string(), "123".to_string()]);
        assert_eq!(target.kind(), "github-issue");
        assert_eq!(target.slug(), "issue-123");
    }

    #[test]
    fn target_parses_pull_request_alias() {
        let target = Target::from_parts(vec!["pr".to_string(), "456".to_string()]);
        assert_eq!(target.kind(), "github-pr");
        assert_eq!(target.slug(), "pr-456");
    }

    #[test]
    fn rewrite_target_alias_preserves_pull_request_target() {
        let rewritten = rewrite_target_alias(vec![
            "pr".to_string(),
            "456".to_string(),
            "--repo".to_string(),
            ".".to_string(),
        ]);

        assert_eq!(
            rewritten,
            vec![
                "pr".to_string(),
                "456".to_string(),
                "--repo".to_string(),
                ".".to_string(),
            ]
        );
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
    fn parsed_run_parse_rejects_missing_task_unknown_flag_and_missing_flag_value() {
        assert!(ParsedRun::parse(Vec::new(), false).is_err());
        assert!(
            ParsedRun::parse(vec!["task".to_string(), "--unknown".to_string()], false).is_err()
        );
        assert!(ParsedRun::parse(vec!["task".to_string(), "--repo".to_string()], false).is_err());
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
        assert!(yaml.contains("# Generated by `valkyrie defaults export --repo`."));
        assert!(yaml.contains("validation:\n  command: cargo test"));

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn effective_settings_selects_write_modes_from_cli_flags() {
        let defaults = DefaultsResolver {
            repo: DefaultsStore::default(),
            user: DefaultsStore::default(),
        };

        let mut parsed =
            ParsedRun::parse(vec!["task".to_string(), "--no-write".to_string()], false).unwrap();
        let settings = EffectiveSettings::from_inputs(&parsed, &defaults);
        assert_eq!(settings.write_mode, WriteMode::NoWrite);
        assert_eq!(settings.write_mode_source, "cli");

        parsed = ParsedRun::parse(vec!["task".to_string(), "--commit".to_string()], false).unwrap();
        let settings = EffectiveSettings::from_inputs(&parsed, &defaults);
        assert_eq!(settings.write_mode, WriteMode::Commit);

        parsed = ParsedRun::parse(vec!["task".to_string(), "--push".to_string()], false).unwrap();
        let settings = EffectiveSettings::from_inputs(&parsed, &defaults);
        assert_eq!(settings.write_mode, WriteMode::Push);

        parsed =
            ParsedRun::parse(vec!["task".to_string(), "--open-pr".to_string()], false).unwrap();
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
    fn skipped_validation_report_is_not_failed() {
        let report = ValidationReport::skipped();
        assert!(!report.failed());
        assert!(report.to_markdown().contains("Validation was skipped"));
    }
}
