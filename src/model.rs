use std::fmt;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    LocalTask { prompt: String },
    Issue { number: u64 },
    PullRequest { number: u64, fix: bool },
    Ci { pr_number: u64, fix: bool },
}

impl Target {
    pub fn display_name(&self) -> String {
        match self {
            Self::LocalTask { prompt } => format!("local task: {prompt}"),
            Self::Issue { number } => format!("issue #{number}"),
            Self::PullRequest { number, fix } => {
                if *fix {
                    format!("pull request #{number} (fix)")
                } else {
                    format!("pull request #{number}")
                }
            }
            Self::Ci { pr_number, fix } => {
                if *fix {
                    format!("CI for PR #{pr_number} (fix)")
                } else {
                    format!("CI for PR #{pr_number}")
                }
            }
        }
    }

    pub fn slug(&self) -> String {
        match self {
            Self::LocalTask { prompt } => slugify(prompt),
            Self::Issue { number } => format!("issue-{number}"),
            Self::PullRequest { number, .. } => format!("pr-{number}"),
            Self::Ci { pr_number, .. } => format!("ci-pr-{pr_number}"),
        }
    }

    pub fn remote_kind(&self) -> bool {
        !matches!(self, Self::LocalTask { .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteMode {
    NoWrite,
    LocalPatch,
    Commit,
    Push,
    PullRequest,
}

impl fmt::Display for WriteMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::NoWrite => "no-write",
            Self::LocalPatch => "local-patch",
            Self::Commit => "commit",
            Self::Push => "push",
            Self::PullRequest => "pr",
        };
        write!(f, "{value}")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcedValue<T> {
    pub value: T,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectiveSettings {
    pub repo_path: SourcedValue<PathBuf>,
    pub base_branch: SourcedValue<String>,
    pub validation_commands: Vec<SourcedValue<String>>,
    pub write_commit: SourcedValue<bool>,
    pub write_push: SourcedValue<bool>,
    pub write_open_pr: SourcedValue<bool>,
    pub write_post_comment: SourcedValue<bool>,
    pub max_iterations: SourcedValue<u32>,
    pub max_files_changed: SourcedValue<u32>,
    pub timeout_minutes: SourcedValue<u64>,
    pub dry_run: bool,
    pub skip_validation: bool,
    pub verbose: bool,
    pub json: bool,
    pub tui: bool,
    pub write_mode: WriteMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum RunState {
    Planned,
    WaitingForApproval,
    Changed,
    Validated,
    Committed,
    Pushed,
    PrOpened,
    PrUpdated,
    Commented,
    Blocked,
    Failed,
    Cancelled,
    Paused,
    Resumed,
}

impl fmt::Display for RunState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Planned => "planned",
            Self::WaitingForApproval => "waiting_for_approval",
            Self::Changed => "changed",
            Self::Validated => "validated",
            Self::Committed => "committed",
            Self::Pushed => "pushed",
            Self::PrOpened => "pr_opened",
            Self::PrUpdated => "pr_updated",
            Self::Commented => "commented",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Paused => "paused",
            Self::Resumed => "resumed",
        };
        write!(f, "{value}")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunResult {
    pub run_id: String,
    pub state: RunState,
    pub target: Target,
    pub summary: String,
    pub repo_path: PathBuf,
    pub run_path: PathBuf,
}

pub fn slugify(input: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;

    for character in input.chars().flat_map(|value| value.to_lowercase()) {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }

    let trimmed = slug.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "task".to_string()
    } else if trimmed.len() > 48 {
        trimmed[..48].trim_matches('-').to_string()
    } else {
        trimmed
    }
}
