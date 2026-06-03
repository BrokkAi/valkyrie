use std::path::Path;
use std::process::Command;

use crate::model::RunState;

pub type AppResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationOutcome {
    pub command: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationSummary {
    pub state: RunState,
    pub outcomes: Vec<ValidationOutcome>,
}

pub fn run_validations(repo_path: &Path, commands: &[String]) -> AppResult<ValidationSummary> {
    let mut outcomes = Vec::new();
    let mut state = RunState::Validated;

    for command in commands {
        let output = Command::new("sh")
            .arg("-lc")
            .arg(command)
            .current_dir(repo_path)
            .output()?;

        let success = output.status.success();
        if !success {
            state = RunState::Failed;
        }

        outcomes.push(ValidationOutcome {
            command: command.clone(),
            success,
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });

        if !success {
            break;
        }
    }

    Ok(ValidationSummary { state, outcomes })
}

pub fn render_validation_report(summary: &ValidationSummary) -> String {
    if summary.outcomes.is_empty() {
        return "# Validation\n\nNo validation commands executed.\n".to_string();
    }

    let mut report = String::from("# Validation\n\n");
    for outcome in &summary.outcomes {
        report.push_str(&format!(
            "## {}\n\n- success: {}\n- exit_code: {}\n\n### stdout\n\n```\n{}\n```\n\n### stderr\n\n```\n{}\n```\n\n",
            outcome.command,
            outcome.success,
            outcome
                .exit_code
                .map(|value| value.to_string())
                .unwrap_or_else(|| "signal".to_string()),
            outcome.stdout.trim_end(),
            outcome.stderr.trim_end(),
        ));
    }
    report
}
