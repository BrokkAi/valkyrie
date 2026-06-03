use std::env;
use std::path::{Path, PathBuf};

use crate::artifacts::{
    RunPaths, doctor_report, read_text, resolve_run_path, write_effective_settings, write_event,
    write_placeholder_diff, write_plan, write_result, write_summary, write_target,
    write_validation,
};
use crate::cli::{CliArgs, Command, DefaultsCommand, RunRequest};
use crate::defaults::{DefaultsFile, repo_defaults_path, resolve_settings, user_defaults_path};
use crate::model::{RunResult, RunState};
use crate::planner::build_plan;
use crate::validation::{ValidationSummary, render_validation_report, run_validations};

pub type AppResult<T> = Result<T, Box<dyn std::error::Error>>;

pub fn run() -> AppResult<()> {
    let cli = CliArgs::parse_env().map_err(to_boxed_error)?;

    match cli.command {
        Command::Run(request)
        | Command::Plan(request)
        | Command::Issue(request)
        | Command::PullRequest(request)
        | Command::Ci(request) => {
            let json = request.options.json;
            let result = execute_run(request)?;
            print_run_result(&result, json);
        }
        Command::Status(run_id) => {
            let repo_path = current_repo_path()?;
            print_status(&repo_path, run_id.as_deref().unwrap_or("latest"))?;
        }
        Command::Logs(run_id) => {
            let repo_path = current_repo_path()?;
            let path = resolve_run_path(&repo_path, &run_id)?.join("events.jsonl");
            print!("{}", read_text(&path)?);
        }
        Command::Diff(run_id) => {
            let repo_path = current_repo_path()?;
            let path = resolve_run_path(&repo_path, &run_id)?.join("diff.patch");
            print!("{}", read_text(&path)?);
        }
        Command::Defaults(command) => handle_defaults(command)?,
        Command::Doctor => {
            let repo_path = current_repo_path()?;
            println!("{}", doctor_report(&repo_path));
        }
        Command::Tui(run_id) => {
            let label = run_id.unwrap_or_else(|| "latest".to_string());
            println!("TUI attach is not implemented yet. Requested run: {label}");
        }
        Command::Attach(run_id) => {
            println!("Attach is not implemented yet. Requested run: {run_id}");
        }
        Command::Resume(run_id) => {
            println!("Resume is not implemented yet. Requested run: {run_id}");
        }
        Command::Replay(run_id) => {
            println!("Replay is not implemented yet. Requested run: {run_id}");
        }
    }

    Ok(())
}

fn execute_run(request: RunRequest) -> AppResult<RunResult> {
    let repo_path = request.options.repo.clone().unwrap_or(current_repo_path()?);
    let repo_path = repo_path.canonicalize().unwrap_or(repo_path);

    let settings = resolve_settings(repo_path.clone(), &request.options)?;
    let (run_id, paths) = RunPaths::create(&repo_path, &request.target)?;

    write_target(&paths, &request.target)?;
    write_effective_settings(&paths, &settings)?;
    write_event(&paths, "run_created", &format!("created run {run_id}"))?;

    let plan = build_plan(&request.target, &settings, &repo_path);
    write_plan(&paths, &plan)?;
    write_event(&paths, "plan_written", "wrote plan.md")?;

    let validation = if should_run_validation(&request, &settings) {
        let commands = settings
            .validation_commands
            .iter()
            .map(|command| command.value.clone())
            .collect::<Vec<_>>();
        let summary = run_validations(&repo_path, &commands)?;
        write_event(
            &paths,
            "validation_finished",
            &format!("validation state {}", summary.state),
        )?;
        summary
    } else {
        write_event(&paths, "validation_skipped", "validation was skipped")?;
        ValidationSummary {
            state: RunState::Planned,
            outcomes: Vec::new(),
        }
    };

    write_validation(&paths, &render_validation_report(&validation))?;
    write_placeholder_diff(&paths)?;

    let state = determine_final_state(&request, &validation);
    let result = RunResult {
        run_id,
        state,
        target: request.target.clone(),
        summary: paths.summary_md.display().to_string(),
        repo_path,
        run_path: paths.root.clone(),
    };

    write_summary(&paths, &result, &settings, &validation)?;
    write_result(&paths, &result)?;
    write_event(
        &paths,
        "run_finished",
        &format!("run finished with {}", result.state),
    )?;

    Ok(result)
}

fn should_run_validation(request: &RunRequest, settings: &crate::model::EffectiveSettings) -> bool {
    !request.options.skip_validation
        && !request.options.dry_run
        && !request.options.no_write
        && !settings.validation_commands.is_empty()
}

fn determine_final_state(request: &RunRequest, validation: &ValidationSummary) -> RunState {
    if validation.state == RunState::Failed {
        return RunState::Failed;
    }

    if request.target.remote_kind() {
        return RunState::Blocked;
    }

    if validation.outcomes.is_empty() {
        RunState::Planned
    } else {
        RunState::Validated
    }
}

fn handle_defaults(command: DefaultsCommand) -> AppResult<()> {
    let repo_path = current_repo_path()?;
    let path = repo_defaults_path(&repo_path);
    let mut defaults = DefaultsFile::load(&path)?;

    match command {
        DefaultsCommand::Get(Some(key)) => {
            if let Some(values) = defaults.get(&key) {
                for value in values {
                    println!("{key} = {value}");
                }
            } else {
                println!("No value found for `{key}`.");
            }
        }
        DefaultsCommand::Get(None) => {
            println!("{}", defaults.render_human());
            println!("\nUser defaults path: {}", user_defaults_path().display());
        }
        DefaultsCommand::Set { key, value } => {
            defaults.set(&key, value);
            defaults.save(&path)?;
            println!("Saved defaults to {}", path.display());
        }
        DefaultsCommand::Unset { key } => {
            defaults.unset(&key);
            defaults.save(&path)?;
            println!("Updated defaults at {}", path.display());
        }
        DefaultsCommand::Export => {
            println!("{}", defaults.export_yaml());
        }
    }

    Ok(())
}

fn print_status(repo_path: &Path, run_id: &str) -> AppResult<()> {
    let run_path = resolve_run_path(repo_path, run_id)?;
    let result_path = run_path.join("result.json");
    let result = read_text(&result_path)?;
    let summary = read_text(&run_path.join("summary.md"))?;
    println!("Run record: {}", result_path.display());
    println!("{result}");
    println!("{summary}");
    Ok(())
}

fn print_run_result(result: &RunResult, json: bool) {
    if json {
        println!(
            "{{\"run_id\":\"{}\",\"state\":\"{}\",\"target\":\"{}\",\"summary\":\"{}\"}}",
            escape_json(&result.run_id),
            result.state,
            escape_json(&result.target.display_name()),
            escape_json(&result.summary),
        );
        return;
    }

    println!("Run: {}", result.run_id);
    println!("Target: {}", result.target.display_name());
    println!("State: {}", result.state);
    println!("Summary: {}", result.summary);
}

fn current_repo_path() -> AppResult<PathBuf> {
    let cwd = env::current_dir()?;
    Ok(find_repo_root(&cwd).unwrap_or(cwd))
}

fn to_boxed_error(message: String) -> Box<dyn std::error::Error> {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message).into()
}

fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(path) = current {
        if path.join(".git").exists() {
            return Some(path.to_path_buf());
        }
        current = path.parent();
    }
    None
}

fn escape_json(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::cli::{RunOptions, RunRequest};
    use crate::model::{Target, WriteMode};

    use super::{execute_run, find_repo_root};

    #[test]
    fn execute_run_creates_artifacts() {
        let repo_path = unique_temp_dir("run");
        fs::create_dir_all(repo_path.join("src")).expect("create src");
        fs::write(
            repo_path.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("write cargo");
        fs::write(repo_path.join("src/main.rs"), "fn main() {}\n").expect("write main");

        let result = execute_run(RunRequest {
            target: Target::LocalTask {
                prompt: "inspect repository".into(),
            },
            options: RunOptions {
                repo: Some(repo_path.clone()),
                dry_run: true,
                ..RunOptions::default()
            },
        })
        .expect("run succeeds");

        assert_eq!(result.state, crate::model::RunState::Planned);
        assert!(result.run_path.join("plan.md").exists());
        assert!(result.run_path.join("result.json").exists());

        let settings = crate::defaults::resolve_settings(repo_path, &RunOptions::default())
            .expect("settings resolve");
        assert_eq!(settings.write_mode, WriteMode::LocalPatch);
    }

    #[test]
    fn finds_repo_root_from_nested_directory() {
        let root = unique_temp_dir("repo-root");
        let nested = root.join("src/bin");
        fs::create_dir_all(&nested).expect("create nested");
        fs::create_dir_all(root.join(".git")).expect("create fake git dir");

        let found = find_repo_root(&nested).expect("finds root");
        assert_eq!(found, root);
    }

    fn unique_temp_dir(name: &str) -> std::path::PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("valkyrie-{name}-{suffix}"))
    }
}
