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
    let repo = parsed.repo.canonicalize().map_err(|error| {
        format!(
            "cannot resolve repo path `{}`: {error}",
            parsed.repo.display()
        )
    })?;

    if !repo.join(".git").exists() {
        return Err(format!(
            "`{}` does not look like a git repository",
            repo.display()
        ));
    }

    let defaults = Defaults::load(&repo)?;
    let settings = EffectiveSettings::from_inputs(&parsed, &defaults);
    let run_id = make_run_id(&parsed.target_slug());
    let run_dir = repo.join(".valkyrie").join("runs").join(&run_id);
    fs::create_dir_all(&run_dir).map_err(|error| {
        format!(
            "cannot create run directory `{}`: {error}",
            run_dir.display()
        )
    })?;

    let plan = render_plan(&parsed, &settings, plan_only);
    write_text(
        run_dir.join("target.json"),
        &render_target_json(&parsed, &repo),
    )?;
    write_text(run_dir.join("effective-settings.json"), &settings.to_json())?;
    write_text(run_dir.join("plan.md"), &plan)?;
    write_text(
        run_dir.join("events.jsonl"),
        &format!(
            "{{\"event\":\"run_created\",\"run_id\":\"{}\",\"mode\":\"{}\"}}\n",
            escape_json(&run_id),
            if plan_only { "plan" } else { "local-patch" }
        ),
    )?;

    if parsed.json {
        println!("{}", render_created_json(&run_id, &run_dir, plan_only));
    } else {
        println!("Run created: {run_id}");
        println!("Artifacts: {}", run_dir.display());
        println!();
        println!("{plan}");
    }

    if plan_only || parsed.no_write || parsed.dry_run {
        write_text(run_dir.join("diff.patch"), "")?;
        write_text(
            run_dir.join("summary.md"),
            "# Summary\n\nPlanning completed. No files were modified by Valkyrie.\n",
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
    write_text(
        run_dir.join("summary.md"),
        "# Summary\n\nRun record created. Agent execution is not wired yet; this MVP skeleton records the target, settings, plan, logs, and current diff.\n",
    )?;
    write_text(
        run_dir.join("result.json"),
        &format!(
            "{{\n  \"run_id\": \"{}\",\n  \"state\": \"planned\",\n  \"run_dir\": \"{}\",\n  \"agent_invoked\": false\n}}\n",
            escape_json(&run_id),
            escape_json(&run_dir.display().to_string())
        ),
    )?;

    Ok(())
}

fn command_plan(args: Vec<String>) -> Result<(), String> {
    if args.is_empty() {
        return Err("usage: valkyrie plan <task>|issue <number> [--repo <path>]".to_string());
    }

    let task = if args[0] == "issue" && args.len() > 1 {
        let mut rewritten = vec![format!("issue {}", args[1])];
        rewritten.extend(args.into_iter().skip(2));
        rewritten
    } else {
        args
    };

    command_run(task, true)
}

fn command_defaults(mut args: Vec<String>) -> Result<(), String> {
    if args.is_empty() {
        return Err("usage: valkyrie defaults <get|set|unset|export> [key] [value]".to_string());
    }

    let action = args.remove(0);
    let repo = current_repo()?;
    let mut defaults = Defaults::load(&repo)?;

    match action.as_str() {
        "get" => {
            if let Some(key) = args.first() {
                match defaults.values.get(key) {
                    Some(value) => println!("{value}"),
                    None => return Err(format!("default `{key}` is not set")),
                }
            } else if defaults.values.is_empty() {
                println!("No repo defaults set.");
            } else {
                for (key, value) in &defaults.values {
                    println!("{key}={value}");
                }
            }
        }
        "set" => {
            if args.len() < 2 {
                return Err("usage: valkyrie defaults set <key> <value>".to_string());
            }
            let key = args.remove(0);
            let value = args.join(" ");
            defaults.values.insert(key.clone(), value.clone());
            defaults.save(&repo)?;
            println!("Set repo default {key}={value}");
        }
        "unset" => {
            let Some(key) = args.first() else {
                return Err("usage: valkyrie defaults unset <key>".to_string());
            };
            defaults.values.remove(key);
            defaults.save(&repo)?;
            println!("Unset repo default {key}");
        }
        "export" => {
            print!("{}", defaults.to_yaml());
            io::stdout().flush().map_err(|error| error.to_string())?;
        }
        other => return Err(format!("unknown defaults action `{other}`")),
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
    Ok(())
}

#[derive(Debug)]
struct ParsedRun {
    task: String,
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
                flag if flag.starts_with('-') => return Err(format!("unknown flag `{flag}`")),
                value => task_parts.push(value.to_string()),
            }
        }

        if task_parts.is_empty() {
            return Err("usage: valkyrie run <task> [--repo <path>]".to_string());
        }

        Ok(Self {
            task: task_parts.join(" "),
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
            validations,
        })
    }

    fn target_slug(&self) -> String {
        self.task
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
}

#[derive(Default)]
struct Defaults {
    values: BTreeMap<String, String>,
}

impl Defaults {
    fn load(repo: &Path) -> Result<Self, String> {
        let path = repo.join(".valkyrie").join("defaults.env");
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&path)
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

    fn save(&self, repo: &Path) -> Result<(), String> {
        let dir = repo.join(".valkyrie");
        fs::create_dir_all(&dir)
            .map_err(|error| format!("cannot create `{}`: {error}", dir.display()))?;
        let mut content = String::from(
            "# Generated by `valkyrie defaults set`. Prefer CLI commands over hand editing.\n",
        );
        for (key, value) in &self.values {
            content.push_str(key);
            content.push('=');
            content.push_str(value);
            content.push('\n');
        }
        write_text(dir.join("defaults.env"), &content)
    }

    fn to_yaml(&self) -> String {
        let mut yaml = String::from(
            "# Generated by `valkyrie defaults export`.\n# Prefer `valkyrie defaults set <key> <value>` over hand-editing.\n",
        );
        for (key, value) in &self.values {
            yaml.push_str(&format!("{}: {}\n", key.replace('.', ":\n  "), value));
        }
        yaml
    }
}

struct EffectiveSettings {
    validation: Vec<(String, &'static str)>,
    write_mode: &'static str,
    commit: (bool, &'static str),
    push: (bool, &'static str),
    open_pr: (bool, &'static str),
    post_comment: (bool, &'static str),
    verbose: bool,
}

impl EffectiveSettings {
    fn from_inputs(parsed: &ParsedRun, defaults: &Defaults) -> Self {
        let mut validation = Vec::new();
        for command in &parsed.validations {
            validation.push((command.clone(), "cli"));
        }
        if validation.is_empty() {
            if let Some(command) = defaults.values.get("validation.command") {
                validation.push((command.clone(), "repo default"));
            }
        }
        if validation.is_empty() {
            validation.push(("cargo test".to_string(), "inferred"));
        }

        let write_mode = if parsed.no_write || parsed.dry_run {
            "no-write"
        } else if parsed.open_pr {
            "pr"
        } else if parsed.push {
            "push"
        } else if parsed.commit {
            "commit"
        } else if parsed.write {
            "local-patch"
        } else {
            "local-patch"
        };

        Self {
            validation,
            write_mode,
            commit: (parsed.commit, if parsed.commit { "cli" } else { "default" }),
            push: (parsed.push, if parsed.push { "cli" } else { "default" }),
            open_pr: (
                parsed.open_pr,
                if parsed.open_pr { "cli" } else { "default" },
            ),
            post_comment: (
                parsed.post_comment,
                if parsed.post_comment {
                    "cli"
                } else {
                    "default"
                },
            ),
            verbose: parsed.verbose,
        }
    }

    fn to_json(&self) -> String {
        let validation = self
            .validation
            .iter()
            .map(|(command, source)| {
                format!(
                    "    {{ \"command\": \"{}\", \"source\": \"{}\" }}",
                    escape_json(command),
                    source
                )
            })
            .collect::<Vec<_>>()
            .join(",\n");
        format!(
            "{{\n  \"write_mode\": \"{}\",\n  \"validation\": [\n{}\n  ],\n  \"commit\": {{ \"value\": {}, \"source\": \"{}\" }},\n  \"push\": {{ \"value\": {}, \"source\": \"{}\" }},\n  \"open_pr\": {{ \"value\": {}, \"source\": \"{}\" }},\n  \"post_comment\": {{ \"value\": {}, \"source\": \"{}\" }},\n  \"verbose\": {}\n}}\n",
            self.write_mode,
            validation,
            self.commit.0,
            self.commit.1,
            self.push.0,
            self.push.1,
            self.open_pr.0,
            self.open_pr.1,
            self.post_comment.0,
            self.post_comment.1,
            self.verbose,
        )
    }
}

fn render_plan(parsed: &ParsedRun, settings: &EffectiveSettings, plan_only: bool) -> String {
    let validation = settings
        .validation
        .iter()
        .map(|(command, source)| format!("- `{command}` ({source})"))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "# Valkyrie Plan\n\n## Problem statement\n\n{}\n\n## Proposed execution\n\n- Resolve local repository context.\n- Record effective settings and run artifacts.\n- Prepare for agent execution through anvil and code intelligence through bifrost.\n\n## Validation\n\n{}\n\n## Write mode\n\n- `{}`{}\n\n## Stop conditions\n\n- Stop before remote writes unless explicit flags are present.\n- Stop if validation fails repeatedly.\n- Stop if file-change limits are exceeded.\n",
        parsed.task,
        validation,
        settings.write_mode,
        if plan_only { " (planning only)" } else { "" }
    )
}

fn render_target_json(parsed: &ParsedRun, repo: &Path) -> String {
    format!(
        "{{\n  \"kind\": \"local-task\",\n  \"task\": \"{}\",\n  \"repo\": \"{}\"\n}}\n",
        escape_json(&parsed.task),
        escape_json(&repo.display().to_string())
    )
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

fn print_help() {
    println!(
        "Valkyrie automation CLI\n\nUsage:\n  valkyrie run <task> [--repo <path>] [--validate <command>] [--no-write|--write] [--json]\n  valkyrie plan <task> [--repo <path>]\n  valkyrie defaults get [key]\n  valkyrie defaults set <key> <value>\n  valkyrie defaults unset <key>\n  valkyrie defaults export\n  valkyrie status <run-id|latest>\n  valkyrie logs <run-id|latest>\n  valkyrie diff <run-id|latest>\n  valkyrie doctor\n\nThis first milestone records run artifacts under .valkyrie/runs and keeps remote writes disabled by default."
    );
}
