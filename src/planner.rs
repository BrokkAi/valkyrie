use std::fs;
use std::path::Path;

use crate::model::{EffectiveSettings, Target};

pub fn build_plan(target: &Target, settings: &EffectiveSettings, repo_path: &Path) -> String {
    let relevant_files = suggest_relevant_files(repo_path);
    let validation_lines = if settings.skip_validation {
        vec!["- Validation skipped by flag".to_string()]
    } else if settings.validation_commands.is_empty() {
        vec!["- No validation commands resolved".to_string()]
    } else {
        settings
            .validation_commands
            .iter()
            .map(|command| format!("- {} ({})", command.value, command.source))
            .collect()
    };

    let remote_note = if target.remote_kind() {
        "\n## Current limitation\n\n- Remote target resolution is not yet wired to GitHub APIs in this MVP.\n- The run records the target, effective settings, and a bounded plan so follow-on work can attach to it.\n"
    } else {
        ""
    };

    format!(
        "# Plan\n\n## Problem statement\n\n- Target: {}\n- Repo: {}\n- Write mode: {}\n- Max iterations: {}\n- Max files changed: {}\n- Base branch: {} ({})\n\n## Relevant files\n\n{}\n\n## Proposed changes\n\n- Create or update a bounded implementation for this target.\n- Persist a run record under `.valkyrie/runs/<run-id>`.\n- Capture validation, summary, and result metadata for later inspection.\n{}\n## Validation steps\n\n{}\n\n## Risks\n\n- The agent execution layer is not yet connected to anvil.\n- Remote targets need GitHub metadata resolution before autonomous code changes can be applied.\n- Validation commands are inferred heuristically when no explicit defaults exist.\n\n## Stop conditions\n\n- Stop if the workspace cannot be prepared.\n- Stop if validation repeatedly fails.\n- Stop if configured file-change or iteration limits are exceeded.\n",
        target.display_name(),
        repo_path.display(),
        settings.write_mode,
        settings.max_iterations.value,
        settings.max_files_changed.value,
        settings.base_branch.value,
        settings.base_branch.source,
        relevant_files
            .iter()
            .map(|path| format!("- {}", path.display()))
            .collect::<Vec<_>>()
            .join("\n"),
        remote_note,
        validation_lines.join("\n"),
    )
}

fn suggest_relevant_files(repo_path: &Path) -> Vec<std::path::PathBuf> {
    let candidates = [
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "src/main.rs",
        "src/lib.rs",
        "README.md",
        "PLANS.md",
    ];

    candidates
        .iter()
        .map(|path| repo_path.join(path))
        .filter(|path| path.exists())
        .collect()
}

pub fn detect_repo_summary(repo_path: &Path) -> String {
    let mut lines = Vec::new();

    for candidate in ["Cargo.toml", "package.json", "pyproject.toml"] {
        let path = repo_path.join(candidate);
        if path.exists() {
            lines.push(format!("- detected manifest: {}", path.display()));
        }
    }

    if lines.is_empty() {
        lines.push("- no known manifest detected".to_string());
    }

    if let Ok(entries) = fs::read_dir(repo_path) {
        let count = entries.filter_map(Result::ok).count();
        lines.push(format!("- top-level entries: {count}"));
    }

    lines.join("\n")
}
