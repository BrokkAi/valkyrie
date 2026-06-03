use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::{EffectiveSettings, RunResult, Target};
use crate::planner::detect_repo_summary;
use crate::validation::ValidationSummary;

pub type AppResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Debug)]
pub struct RunPaths {
    pub root: PathBuf,
    pub target_json: PathBuf,
    pub effective_settings_json: PathBuf,
    pub plan_md: PathBuf,
    pub events_jsonl: PathBuf,
    pub diff_patch: PathBuf,
    pub validation_md: PathBuf,
    pub summary_md: PathBuf,
    pub result_json: PathBuf,
}

impl RunPaths {
    pub fn create(repo_path: &Path, target: &Target) -> AppResult<(String, Self)> {
        let run_id = generate_run_id(target);
        let root = repo_path.join(".valkyrie").join("runs").join(&run_id);
        fs::create_dir_all(&root)?;

        let paths = Self {
            target_json: root.join("target.json"),
            effective_settings_json: root.join("effective-settings.json"),
            plan_md: root.join("plan.md"),
            events_jsonl: root.join("events.jsonl"),
            diff_patch: root.join("diff.patch"),
            validation_md: root.join("validation.md"),
            summary_md: root.join("summary.md"),
            result_json: root.join("result.json"),
            root,
        };

        Ok((run_id, paths))
    }
}

pub fn write_target(paths: &RunPaths, target: &Target) -> AppResult<()> {
    fs::write(&paths.target_json, target_json(target))?;
    Ok(())
}

pub fn write_effective_settings(paths: &RunPaths, settings: &EffectiveSettings) -> AppResult<()> {
    fs::write(
        &paths.effective_settings_json,
        effective_settings_json(settings),
    )?;
    Ok(())
}

pub fn write_plan(paths: &RunPaths, content: &str) -> AppResult<()> {
    fs::write(&paths.plan_md, content)?;
    Ok(())
}

pub fn write_event(paths: &RunPaths, name: &str, message: &str) -> AppResult<()> {
    let mut existing = if paths.events_jsonl.exists() {
        fs::read_to_string(&paths.events_jsonl)?
    } else {
        String::new()
    };

    existing.push_str(&format!(
        "{{\"timestamp\":\"{}\",\"event\":\"{}\",\"message\":\"{}\"}}\n",
        unix_timestamp_millis(),
        escape_json(name),
        escape_json(message)
    ));
    fs::write(&paths.events_jsonl, existing)?;
    Ok(())
}

pub fn write_validation(paths: &RunPaths, content: &str) -> AppResult<()> {
    fs::write(&paths.validation_md, content)?;
    Ok(())
}

pub fn write_placeholder_diff(paths: &RunPaths) -> AppResult<()> {
    fs::write(
        &paths.diff_patch,
        "# No patch captured yet.\n# The agent execution layer is not connected in this MVP.\n",
    )?;
    Ok(())
}

pub fn write_summary(
    paths: &RunPaths,
    result: &RunResult,
    settings: &EffectiveSettings,
    validation: &ValidationSummary,
) -> AppResult<()> {
    let validation_line = if validation.outcomes.is_empty() {
        "not run".to_string()
    } else {
        validation.state.to_string()
    };

    let content = format!(
        "# Summary\n\n- Run: {}\n- Target: {}\n- State: {}\n- Repo: {}\n- Write mode: {}\n- Validation: {}\n- Summary path: {}\n- Base branch: {} ({})\n",
        result.run_id,
        result.target.display_name(),
        result.state,
        result.repo_path.display(),
        settings.write_mode,
        validation_line,
        paths.summary_md.display(),
        settings.base_branch.value,
        settings.base_branch.source,
    );

    fs::write(&paths.summary_md, content)?;
    Ok(())
}

pub fn write_result(paths: &RunPaths, result: &RunResult) -> AppResult<()> {
    fs::write(&paths.result_json, result_json(result))?;
    Ok(())
}

pub fn latest_run_path(repo_path: &Path) -> AppResult<Option<PathBuf>> {
    let runs_dir = repo_path.join(".valkyrie").join("runs");
    if !runs_dir.exists() {
        return Ok(None);
    }

    let mut entries = fs::read_dir(runs_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    entries.sort();
    Ok(entries.pop())
}

pub fn resolve_run_path(repo_path: &Path, run_id: &str) -> AppResult<PathBuf> {
    if run_id == "latest" {
        return latest_run_path(repo_path)?.ok_or_else(|| "no runs found".into());
    }
    Ok(repo_path.join(".valkyrie").join("runs").join(run_id))
}

pub fn read_text(path: &Path) -> AppResult<String> {
    Ok(fs::read_to_string(path)?)
}

pub fn doctor_report(repo_path: &Path) -> String {
    format!(
        "Repo: {}\nRun storage: {}\n{}\n",
        repo_path.display(),
        repo_path.join(".valkyrie").join("runs").display(),
        detect_repo_summary(repo_path),
    )
}

fn generate_run_id(target: &Target) -> String {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_millis();
    format!("run-{stamp}-{}", target.slug())
}

fn unix_timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_millis()
}

fn target_json(target: &Target) -> String {
    match target {
        Target::LocalTask { prompt } => format!(
            "{{\n  \"kind\": \"local_task\",\n  \"prompt\": \"{}\"\n}}\n",
            escape_json(prompt)
        ),
        Target::Issue { number } => {
            format!("{{\n  \"kind\": \"issue\",\n  \"number\": {}\n}}\n", number)
        }
        Target::PullRequest { number, fix } => format!(
            "{{\n  \"kind\": \"pull_request\",\n  \"number\": {},\n  \"fix\": {}\n}}\n",
            number, fix
        ),
        Target::Ci { pr_number, fix } => format!(
            "{{\n  \"kind\": \"ci\",\n  \"pr_number\": {},\n  \"fix\": {}\n}}\n",
            pr_number, fix
        ),
    }
}

fn effective_settings_json(settings: &EffectiveSettings) -> String {
    let validation = if settings.validation_commands.is_empty() {
        "[]".to_string()
    } else {
        format!(
            "[\n{}\n  ]",
            settings
                .validation_commands
                .iter()
                .map(|command| format!(
                    "    {{\"command\":\"{}\",\"source\":\"{}\"}}",
                    escape_json(&command.value),
                    escape_json(&command.source)
                ))
                .collect::<Vec<_>>()
                .join(",\n")
        )
    };

    format!(
        concat!(
            "{{\n",
            "  \"repo_path\": {{\"value\": \"{}\", \"source\": \"{}\"}},\n",
            "  \"base_branch\": {{\"value\": \"{}\", \"source\": \"{}\"}},\n",
            "  \"validation_commands\": {},\n",
            "  \"write\": {{\n",
            "    \"mode\": \"{}\",\n",
            "    \"commit\": {{\"value\": {}, \"source\": \"{}\"}},\n",
            "    \"push\": {{\"value\": {}, \"source\": \"{}\"}},\n",
            "    \"open_pr\": {{\"value\": {}, \"source\": \"{}\"}},\n",
            "    \"post_comment\": {{\"value\": {}, \"source\": \"{}\"}}\n",
            "  }},\n",
            "  \"limits\": {{\n",
            "    \"max_iterations\": {{\"value\": {}, \"source\": \"{}\"}},\n",
            "    \"max_files_changed\": {{\"value\": {}, \"source\": \"{}\"}},\n",
            "    \"timeout_minutes\": {{\"value\": {}, \"source\": \"{}\"}}\n",
            "  }},\n",
            "  \"flags\": {{\"dry_run\": {}, \"skip_validation\": {}, \"verbose\": {}, \"json\": {}, \"tui\": {}}}\n",
            "}}\n"
        ),
        escape_json(&settings.repo_path.value.display().to_string()),
        escape_json(&settings.repo_path.source),
        escape_json(&settings.base_branch.value),
        escape_json(&settings.base_branch.source),
        validation,
        settings.write_mode,
        settings.write_commit.value,
        escape_json(&settings.write_commit.source),
        settings.write_push.value,
        escape_json(&settings.write_push.source),
        settings.write_open_pr.value,
        escape_json(&settings.write_open_pr.source),
        settings.write_post_comment.value,
        escape_json(&settings.write_post_comment.source),
        settings.max_iterations.value,
        escape_json(&settings.max_iterations.source),
        settings.max_files_changed.value,
        escape_json(&settings.max_files_changed.source),
        settings.timeout_minutes.value,
        escape_json(&settings.timeout_minutes.source),
        settings.dry_run,
        settings.skip_validation,
        settings.verbose,
        settings.json,
        settings.tui,
    )
}

fn result_json(result: &RunResult) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"run_id\": \"{}\",\n",
            "  \"state\": \"{}\",\n",
            "  \"target\": \"{}\",\n",
            "  \"summary\": \"{}\",\n",
            "  \"repo_path\": \"{}\",\n",
            "  \"run_path\": \"{}\"\n",
            "}}\n"
        ),
        escape_json(&result.run_id),
        result.state,
        escape_json(&result.target.display_name()),
        escape_json(&result.summary),
        escape_json(&result.repo_path.display().to_string()),
        escape_json(&result.run_path.display().to_string()),
    )
}

fn escape_json(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}
