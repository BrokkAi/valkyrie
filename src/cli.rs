use std::env;
use std::path::PathBuf;

use crate::model::Target;

pub type CliResult<T> = Result<T, String>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliArgs {
    pub command: Command,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Run(RunRequest),
    Plan(RunRequest),
    Issue(RunRequest),
    PullRequest(RunRequest),
    Ci(RunRequest),
    Status(Option<String>),
    Logs(String),
    Diff(String),
    Defaults(DefaultsCommand),
    Doctor,
    Tui(Option<String>),
    Attach(String),
    Resume(String),
    Replay(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRequest {
    pub target: Target,
    pub options: RunOptions,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RunOptions {
    pub repo: Option<PathBuf>,
    pub branch: Option<String>,
    pub base: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub profile: Option<String>,
    pub dry_run: bool,
    pub no_write: bool,
    pub write: bool,
    pub commit: bool,
    pub push: bool,
    pub open_pr: bool,
    pub update_pr: bool,
    pub post_comment: bool,
    pub max_iterations: Option<u32>,
    pub max_files: Option<u32>,
    pub timeout_minutes: Option<u64>,
    pub budget: Option<String>,
    pub validation_commands: Vec<String>,
    pub skip_validation: bool,
    pub tui: bool,
    pub json: bool,
    pub verbose: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefaultsCommand {
    Get(Option<String>),
    Set { key: String, value: String },
    Unset { key: String },
    Export,
}

impl CliArgs {
    pub fn parse_env() -> CliResult<Self> {
        let args = env::args().skip(1).collect::<Vec<_>>();
        Self::parse_from(args)
    }

    pub fn parse_from(args: Vec<String>) -> CliResult<Self> {
        let Some(command) = args.first() else {
            return Err(help_text());
        };

        if matches!(command.as_str(), "-h" | "--help" | "help") {
            return Err(help_text());
        }

        match command.as_str() {
            "run" => parse_run_like(CommandKind::Run, &args[1..]),
            "plan" => parse_run_like(CommandKind::Plan, &args[1..]),
            "issue" => parse_issue_like(CommandKind::Issue, &args[1..]),
            "pr" => parse_issue_like(CommandKind::PullRequest, &args[1..]),
            "ci" => parse_ci(&args[1..]),
            "status" => Ok(Self {
                command: Command::Status(args.get(1).cloned()),
            }),
            "logs" => Ok(Self {
                command: Command::Logs(required_arg(&args, 1, "logs requires a run id")?),
            }),
            "diff" => Ok(Self {
                command: Command::Diff(required_arg(&args, 1, "diff requires a run id")?),
            }),
            "defaults" => parse_defaults(&args[1..]),
            "doctor" => Ok(Self {
                command: Command::Doctor,
            }),
            "tui" => Ok(Self {
                command: Command::Tui(args.get(1).cloned()),
            }),
            "attach" => Ok(Self {
                command: Command::Attach(required_arg(&args, 1, "attach requires a run id")?),
            }),
            "resume" => Ok(Self {
                command: Command::Resume(required_arg(&args, 1, "resume requires a run id")?),
            }),
            "replay" => Ok(Self {
                command: Command::Replay(required_arg(&args, 1, "replay requires a run id")?),
            }),
            other => Err(format!("unknown command `{other}`\n\n{}", help_text())),
        }
    }
}

enum CommandKind {
    Run,
    Plan,
    Issue,
    PullRequest,
}

fn parse_run_like(kind: CommandKind, args: &[String]) -> CliResult<CliArgs> {
    let split_at = args
        .iter()
        .position(|item| item.starts_with("--"))
        .unwrap_or(args.len());
    let positional = &args[..split_at];
    let option_tokens = &args[split_at..];

    if positional.is_empty() {
        return Err("missing target".to_string());
    }

    let mut options = parse_run_options(option_tokens)?;
    if matches!(kind, CommandKind::Plan) {
        options.no_write = true;
        options.dry_run = true;
    }

    let target = parse_run_target(positional)?;
    let command = match kind {
        CommandKind::Run => Command::Run(RunRequest { target, options }),
        CommandKind::Plan => Command::Plan(RunRequest { target, options }),
        CommandKind::Issue => Command::Issue(RunRequest { target, options }),
        CommandKind::PullRequest => Command::PullRequest(RunRequest { target, options }),
    };

    Ok(CliArgs { command })
}

fn parse_issue_like(kind: CommandKind, args: &[String]) -> CliResult<CliArgs> {
    let split_at = args
        .iter()
        .position(|item| item.starts_with("--"))
        .unwrap_or(args.len());
    let positional = &args[..split_at];
    let option_tokens = &args[split_at..];

    if positional.is_empty() {
        return Err("missing numeric identifier".to_string());
    }

    let number = positional[0]
        .parse::<u64>()
        .map_err(|_| format!("invalid numeric identifier `{}`", positional[0]))?;

    let mut options = parse_run_options(option_tokens)?;
    let target = match kind {
        CommandKind::Issue => Target::Issue { number },
        CommandKind::PullRequest => Target::PullRequest {
            number,
            fix: options.write || options.commit || option_tokens.contains(&"--fix".to_string()),
        },
        _ => return Err("unsupported command shape".to_string()),
    };

    if option_tokens.contains(&"--fix".to_string()) {
        options.write = true;
    }

    let command = match kind {
        CommandKind::Issue => Command::Issue(RunRequest { target, options }),
        CommandKind::PullRequest => Command::PullRequest(RunRequest { target, options }),
        _ => unreachable!(),
    };

    Ok(CliArgs { command })
}

fn parse_ci(args: &[String]) -> CliResult<CliArgs> {
    let options = parse_run_options(args)?;
    let mut pr_number = None;
    let mut fix = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--pr" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| "missing value for --pr".to_string())?;
                pr_number = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid PR number `{value}`"))?,
                );
            }
            "--fix" => fix = true,
            flag if flag.starts_with("--") => {}
            other => return Err(format!("unexpected CI argument `{other}`")),
        }
        index += 1;
    }

    let Some(number) = pr_number else {
        return Err("ci requires --pr <number>".to_string());
    };

    Ok(CliArgs {
        command: Command::Ci(RunRequest {
            target: Target::Ci {
                pr_number: number,
                fix,
            },
            options,
        }),
    })
}

fn parse_defaults(args: &[String]) -> CliResult<CliArgs> {
    let Some(subcommand) = args.first() else {
        return Err("defaults requires a subcommand".to_string());
    };

    let command = match subcommand.as_str() {
        "get" => DefaultsCommand::Get(args.get(1).cloned()),
        "set" => DefaultsCommand::Set {
            key: args
                .get(1)
                .cloned()
                .ok_or_else(|| "defaults set requires a key".to_string())?,
            value: args
                .get(2)
                .cloned()
                .ok_or_else(|| "defaults set requires a value".to_string())?,
        },
        "unset" => DefaultsCommand::Unset {
            key: args
                .get(1)
                .cloned()
                .ok_or_else(|| "defaults unset requires a key".to_string())?,
        },
        "export" => DefaultsCommand::Export,
        other => return Err(format!("unknown defaults subcommand `{other}`")),
    };

    Ok(CliArgs {
        command: Command::Defaults(command),
    })
}

fn parse_run_target(positional: &[String]) -> CliResult<Target> {
    match positional {
        [kind, number] if kind == "issue" => Ok(Target::Issue {
            number: number
                .parse::<u64>()
                .map_err(|_| format!("invalid issue number `{number}`"))?,
        }),
        [kind, number] if kind == "pr" => Ok(Target::PullRequest {
            number: number
                .parse::<u64>()
                .map_err(|_| format!("invalid PR number `{number}`"))?,
            fix: false,
        }),
        [single] => Ok(Target::LocalTask {
            prompt: single.clone(),
        }),
        many => Ok(Target::LocalTask {
            prompt: many.join(" "),
        }),
    }
}

fn parse_run_options(tokens: &[String]) -> CliResult<RunOptions> {
    let mut options = RunOptions::default();
    let mut index = 0;

    while index < tokens.len() {
        match tokens[index].as_str() {
            "--repo" => options.repo = Some(PathBuf::from(read_value(tokens, &mut index)?)),
            "--branch" => options.branch = Some(read_value(tokens, &mut index)?),
            "--base" => options.base = Some(read_value(tokens, &mut index)?),
            "--agent" => options.agent = Some(read_value(tokens, &mut index)?),
            "--model" => options.model = Some(read_value(tokens, &mut index)?),
            "--profile" => options.profile = Some(read_value(tokens, &mut index)?),
            "--dry-run" => options.dry_run = true,
            "--no-write" => options.no_write = true,
            "--write" => options.write = true,
            "--commit" => options.commit = true,
            "--push" => options.push = true,
            "--open-pr" => options.open_pr = true,
            "--update-pr" => options.update_pr = true,
            "--post-comment" => options.post_comment = true,
            "--max-iterations" => {
                options.max_iterations = Some(
                    read_value(tokens, &mut index)?
                        .parse()
                        .map_err(invalid_number)?,
                )
            }
            "--max-files" => {
                options.max_files = Some(
                    read_value(tokens, &mut index)?
                        .parse()
                        .map_err(invalid_number)?,
                )
            }
            "--timeout" => {
                options.timeout_minutes = Some(
                    read_value(tokens, &mut index)?
                        .parse()
                        .map_err(invalid_number)?,
                )
            }
            "--budget" => options.budget = Some(read_value(tokens, &mut index)?),
            "--validate" => options
                .validation_commands
                .push(read_value(tokens, &mut index)?),
            "--skip-validation" => options.skip_validation = true,
            "--tui" => options.tui = true,
            "--json" => options.json = true,
            "--verbose" => options.verbose = true,
            "--fix" => options.write = true,
            flag => return Err(format!("unknown flag `{flag}`")),
        }
        index += 1;
    }

    Ok(options)
}

fn read_value(tokens: &[String], index: &mut usize) -> CliResult<String> {
    *index += 1;
    tokens
        .get(*index)
        .cloned()
        .ok_or_else(|| "missing flag value".to_string())
}

fn invalid_number(error: impl std::fmt::Display) -> String {
    format!("invalid numeric value: {error}")
}

fn required_arg(args: &[String], index: usize, message: &str) -> CliResult<String> {
    args.get(index).cloned().ok_or_else(|| message.to_string())
}

fn help_text() -> String {
    [
        "Valkyrie MVP",
        "",
        "Commands:",
        "  valkyrie run <task>",
        "  valkyrie run issue <number>",
        "  valkyrie issue <number>",
        "  valkyrie pr <number> [--fix]",
        "  valkyrie ci --pr <number> [--fix]",
        "  valkyrie plan <target>",
        "  valkyrie status [run-id|latest]",
        "  valkyrie logs <run-id|latest>",
        "  valkyrie diff <run-id|latest>",
        "  valkyrie defaults get [key]",
        "  valkyrie defaults set <key> <value>",
        "  valkyrie defaults unset <key>",
        "  valkyrie defaults export",
        "  valkyrie doctor",
        "",
        "Common flags:",
        "  --repo <path> --validate <cmd> --skip-validation --dry-run --no-write",
        "  --commit --push --open-pr --post-comment --max-iterations <n>",
        "  --max-files <n> --timeout <minutes> --tui --json --verbose",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{CliArgs, Command, DefaultsCommand};
    use crate::model::Target;

    #[test]
    fn parses_run_with_flags() {
        let parsed = CliArgs::parse_from(vec![
            "run".into(),
            "fix parser panic".into(),
            "--validate".into(),
            "cargo test".into(),
            "--commit".into(),
            "--max-iterations".into(),
            "3".into(),
        ])
        .expect("parses");

        let Command::Run(request) = parsed.command else {
            panic!("expected run command");
        };

        assert_eq!(
            request.target,
            Target::LocalTask {
                prompt: "fix parser panic".into()
            }
        );
        assert_eq!(request.options.validation_commands, vec!["cargo test"]);
        assert!(request.options.commit);
        assert_eq!(request.options.max_iterations, Some(3));
    }

    #[test]
    fn parses_issue_shortcut() {
        let parsed = CliArgs::parse_from(vec![
            "issue".into(),
            "123".into(),
            "--repo".into(),
            ".".into(),
        ])
        .expect("parses");

        let Command::Issue(request) = parsed.command else {
            panic!("expected issue command");
        };
        assert_eq!(request.target, Target::Issue { number: 123 });
    }

    #[test]
    fn parses_defaults_get() {
        let parsed = CliArgs::parse_from(vec!["defaults".into(), "get".into()]).expect("parses");
        let Command::Defaults(DefaultsCommand::Get(None)) = parsed.command else {
            panic!("expected defaults get");
        };
    }
}
