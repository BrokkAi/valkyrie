use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::str::FromStr;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{CommandFactory, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    match cli.command {
        Some(CommandKind::Watch(args)) => run_watch(args),
        Some(CommandKind::Doctor) => run_doctor(),
        None => {
            Cli::command()
                .print_help()
                .map_err(|error| error.to_string())?;
            println!();
            Ok(())
        }
    }
}

#[derive(Parser)]
#[command(
    name = "vk",
    version,
    about = "Watch GitHub pull requests and review them with Anvil"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<CommandKind>,
}

#[derive(Subcommand)]
enum CommandKind {
    /// Poll GitHub repositories for open pull requests and review new ones.
    Watch(WatchArgs),
    /// Check local runtime dependencies.
    Doctor,
}

#[derive(Parser, Debug)]
struct WatchArgs {
    /// Repository to watch, in owner/name form. Can be passed more than once.
    #[arg(short = 'r', long = "repo", value_name = "OWNER/REPO")]
    repos: Vec<RepoSlug>,

    /// Additional repositories to watch, in owner/name form.
    #[arg(value_name = "OWNER/REPO")]
    positional_repos: Vec<RepoSlug>,

    /// Polling interval in seconds.
    #[arg(long, default_value_t = 60)]
    interval_seconds: u64,

    /// Local directory used to remember reviewed pull requests.
    #[arg(long, default_value = ".valkyrie")]
    state_dir: PathBuf,

    /// Poll once, then exit.
    #[arg(long)]
    once: bool,

    /// Fetch pull requests and build prompts without invoking Anvil or posting reviews.
    #[arg(long)]
    dry_run: bool,

    /// Maximum open pull requests fetched per repository per poll.
    #[arg(long, default_value_t = 50)]
    limit: u8,

    /// Anvil binary to launch as the ACP server.
    #[arg(long, default_value = "anvil")]
    anvil_binary: String,

    /// Optional default model forwarded to Anvil.
    #[arg(long)]
    default_model: Option<String>,

    /// Maximum Anvil tool-calling turns per prompt.
    #[arg(long, default_value_t = 25)]
    max_turns: u16,

    /// Show Anvil stderr logs while reviews are generated.
    #[arg(long)]
    show_anvil_logs: bool,

    /// Show per-repository polling summaries.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
struct RepoSlug {
    owner: String,
    name: String,
}

impl RepoSlug {
    fn as_path(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    fn review_marker(&self, number: u64) -> String {
        format!("<!-- valkyrie-review: {}#{} -->", self.as_path(), number)
    }
}

impl fmt::Display for RepoSlug {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.owner, self.name)
    }
}

impl FromStr for RepoSlug {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (owner, name) = input
            .split_once('/')
            .ok_or_else(|| format!("repository `{input}` must use owner/name form"))?;
        validate_slug_part(owner, "owner")?;
        validate_slug_part(name, "repository")?;
        Ok(Self {
            owner: owner.to_string(),
            name: name.to_string(),
        })
    }
}

fn validate_slug_part(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("repository {label} cannot be empty"));
    }
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_' || byte == b'-')
    {
        Ok(())
    } else {
        Err(format!(
            "repository {label} `{value}` contains unsupported characters"
        ))
    }
}

fn run_watch(args: WatchArgs) -> Result<(), String> {
    let repositories = merge_repositories(args.repos.clone(), args.positional_repos.clone())?;
    let github = GitHubClient::new();
    let viewer = github.current_user()?;
    validate_repositories(&github, &repositories)?;

    fs::create_dir_all(&args.state_dir).map_err(|error| {
        format!(
            "cannot create state directory `{}`: {error}",
            args.state_dir.display()
        )
    })?;
    let state_path = args.state_dir.join("reviews.json");
    let mut state = ReviewState::load(&state_path)?;

    loop {
        for repository in &repositories {
            poll_repository(repository, &args, &github, &viewer, &state_path, &mut state)?;
        }

        if args.once {
            break;
        }

        thread::sleep(Duration::from_secs(args.interval_seconds));
    }

    Ok(())
}

fn merge_repositories(
    mut flag_repos: Vec<RepoSlug>,
    positional_repos: Vec<RepoSlug>,
) -> Result<Vec<RepoSlug>, String> {
    flag_repos.extend(positional_repos);
    if flag_repos.is_empty() {
        return Err("watch requires at least one repository".to_string());
    }

    let mut seen = BTreeSet::new();
    let mut repositories = Vec::new();
    for repository in flag_repos {
        if seen.insert(repository.clone()) {
            repositories.push(repository);
        }
    }
    Ok(repositories)
}

fn validate_repositories(github: &GitHubClient, repositories: &[RepoSlug]) -> Result<(), String> {
    for repository in repositories {
        github.validate_repository(repository)?;
    }
    Ok(())
}

fn poll_repository(
    repository: &RepoSlug,
    args: &WatchArgs,
    github: &GitHubClient,
    viewer: &GitHubUser,
    state_path: &Path,
    state: &mut ReviewState,
) -> Result<(), String> {
    let pulls = github.open_pull_requests(repository, args.limit)?;
    let mut skipped_recorded = 0;
    let mut skipped_existing_review = 0;
    for pull in pulls {
        let state_key = review_state_key(repository, pull.number);
        if state.reviews.contains_key(&state_key) {
            skipped_recorded += 1;
            continue;
        }

        let marker = repository.review_marker(pull.number);
        if github.review_already_posted(repository, pull.number, &viewer.login, &marker)? {
            skipped_existing_review += 1;
            state.record(repository, &pull, None);
            state.save(state_path)?;
            continue;
        }

        let files = github.pull_request_files(repository, pull.number)?;
        let prompt = build_review_prompt(repository, &pull, &files);
        if args.dry_run {
            println!(
                "dry run: would review {repository}#{} with {} changed file(s)",
                pull.number,
                files.len()
            );
            continue;
        }

        println!("reviewing {repository}#{} with Anvil", pull.number);
        let anvil = AnvilOptions {
            binary: args.anvil_binary.clone(),
            default_model: args.default_model.clone(),
            max_turns: args.max_turns,
            show_logs: args.show_anvil_logs,
        };
        let generated = anvil_review(&anvil, &prompt)?;
        let review = sanitize_generated_review(generated, &files)?;
        let posted = github.post_review(repository, &pull, &review, &marker)?;
        state.record(repository, &pull, posted.id);
        state.save(state_path)?;
        println!("posted review for {repository}#{}", pull.number);
    }

    if args.verbose {
        println!(
            "polled {repository}: {} already recorded, {} existing Valkyrie review(s)",
            skipped_recorded, skipped_existing_review
        );
    }
    Ok(())
}

fn run_doctor() -> Result<(), String> {
    println!("gh: {}", command_status("gh"));
    println!("anvil: {}", command_status("anvil"));
    println!("github api: {}", github_api_status());
    Ok(())
}

fn command_status(binary: &str) -> &'static str {
    match Command::new(binary)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => "ok",
        _ => "missing",
    }
}

fn github_api_status() -> &'static str {
    match Command::new("gh")
        .args(["api", "/user", "--method", "GET"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => "ok",
        _ => "unavailable",
    }
}

#[derive(Debug, Deserialize)]
struct GitHubUser {
    login: String,
}

#[derive(Debug, Deserialize)]
struct PullRequest {
    number: u64,
    title: String,
    body: Option<String>,
    html_url: String,
    user: GitHubUser,
    head: PullRequestHead,
}

#[derive(Debug, Deserialize)]
struct PullRequestHead {
    sha: String,
    #[serde(rename = "ref")]
    branch: String,
}

#[derive(Debug, Deserialize)]
struct PullRequestFile {
    filename: String,
    status: String,
    additions: u64,
    deletions: u64,
    patch: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PullRequestReview {
    user: GitHubUser,
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PostedReview {
    id: Option<u64>,
}

struct GitHubClient {}

impl GitHubClient {
    fn new() -> Self {
        Self {}
    }

    fn current_user(&self) -> Result<GitHubUser, String> {
        self.get("/user")
    }

    fn validate_repository(&self, repository: &RepoSlug) -> Result<(), String> {
        let endpoint = format!("/repos/{}/{}", repository.owner, repository.name);
        self.get::<Value>(&endpoint)
            .map(|_| ())
            .map_err(|error| repository_access_error(repository, &error))
    }

    fn open_pull_requests(
        &self,
        repository: &RepoSlug,
        limit: u8,
    ) -> Result<Vec<PullRequest>, String> {
        let endpoint = format!(
            "/repos/{}/{}/pulls?state=open&sort=created&direction=asc&per_page={}",
            repository.owner, repository.name, limit
        );
        self.get(&endpoint)
    }

    fn pull_request_files(
        &self,
        repository: &RepoSlug,
        number: u64,
    ) -> Result<Vec<PullRequestFile>, String> {
        let endpoint = format!(
            "/repos/{}/{}/pulls/{number}/files?per_page=100",
            repository.owner, repository.name
        );
        self.get(&endpoint)
    }

    fn review_already_posted(
        &self,
        repository: &RepoSlug,
        number: u64,
        viewer_login: &str,
        marker: &str,
    ) -> Result<bool, String> {
        let endpoint = format!(
            "/repos/{}/{}/pulls/{number}/reviews?per_page=100",
            repository.owner, repository.name
        );
        let reviews: Vec<PullRequestReview> = self.get(&endpoint)?;
        Ok(reviews.iter().any(|review| {
            review.user.login == viewer_login
                && review
                    .body
                    .as_deref()
                    .is_some_and(|body| body.contains(marker))
        }))
    }

    fn post_review(
        &self,
        repository: &RepoSlug,
        pull: &PullRequest,
        review: &GeneratedReview,
        marker: &str,
    ) -> Result<PostedReview, String> {
        let comments = review
            .comments
            .iter()
            .filter_map(|comment| {
                comment.line.map(|line| {
                    json!({
                        "path": comment.path,
                        "line": line,
                        "side": "RIGHT",
                        "body": comment.body,
                    })
                })
            })
            .collect::<Vec<_>>();
        let body = format!("{}\n\n{}", review.summary.trim(), marker);
        let payload = json!({
            "commit_id": pull.head.sha,
            "body": body,
            "event": "COMMENT",
            "comments": comments,
        });
        let endpoint = format!(
            "/repos/{}/{}/pulls/{}/reviews",
            repository.owner, repository.name, pull.number
        );
        self.post(&endpoint, payload)
    }

    fn get<T: for<'de> Deserialize<'de>>(&self, endpoint: &str) -> Result<T, String> {
        let output = Command::new("gh")
            .args(["api", endpoint, "--method", "GET"])
            .output()
            .map_err(|error| format!("cannot run `gh api`: {error}"))?;
        decode_gh_output(output, endpoint)
    }

    fn post<T: for<'de> Deserialize<'de>>(
        &self,
        endpoint: &str,
        payload: Value,
    ) -> Result<T, String> {
        let mut child = Command::new("gh")
            .args(["api", endpoint, "--method", "POST", "--input", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("cannot run `gh api`: {error}"))?;
        {
            let stdin = child
                .stdin
                .as_mut()
                .ok_or_else(|| "cannot open `gh api` stdin".to_string())?;
            serde_json::to_writer(stdin, &payload)
                .map_err(|error| format!("cannot write GitHub API payload: {error}"))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|error| format!("cannot read `gh api` output: {error}"))?;
        decode_gh_output(output, endpoint)
    }
}

fn decode_gh_output<T: for<'de> Deserialize<'de>>(
    output: std::process::Output,
    endpoint: &str,
) -> Result<T, String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("`gh api {endpoint}` failed: {}", stderr.trim()));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("cannot decode `gh api {endpoint}` response: {error}"))
}

fn repository_access_error(repository: &RepoSlug, error: &str) -> String {
    format!(
        "cannot access repository `{repository}` with `gh api`: {error}. \
         Check that the repository exists and the authenticated GitHub user can read it."
    )
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ReviewState {
    reviews: BTreeMap<String, RecordedReview>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RecordedReview {
    repo: String,
    pull_number: u64,
    head_sha: String,
    github_review_id: Option<u64>,
    posted_at_unix: u64,
}

impl ReviewState {
    fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = fs::read_to_string(path)
            .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
        serde_json::from_str(&contents)
            .map_err(|error| format!("cannot parse `{}`: {error}", path.display()))
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create `{}`: {error}", parent.display()))?;
        }
        let contents = serde_json::to_string_pretty(self)
            .map_err(|error| format!("cannot encode review state: {error}"))?;
        fs::write(path, format!("{contents}\n"))
            .map_err(|error| format!("cannot write `{}`: {error}", path.display()))
    }

    fn record(&mut self, repository: &RepoSlug, pull: &PullRequest, github_review_id: Option<u64>) {
        let key = review_state_key(repository, pull.number);
        self.reviews.insert(
            key,
            RecordedReview {
                repo: repository.as_path(),
                pull_number: pull.number,
                head_sha: pull.head.sha.clone(),
                github_review_id,
                posted_at_unix: now_unix_seconds(),
            },
        );
    }
}

fn review_state_key(repository: &RepoSlug, number: u64) -> String {
    format!("{}#{}", repository.as_path(), number)
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Debug, Serialize, Deserialize)]
struct GeneratedReview {
    summary: String,
    #[serde(default)]
    comments: Vec<GeneratedComment>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GeneratedComment {
    path: String,
    line: Option<u64>,
    body: String,
}

fn sanitize_generated_review(
    mut review: GeneratedReview,
    files: &[PullRequestFile],
) -> Result<GeneratedReview, String> {
    review.summary = review.summary.trim().to_string();
    if review.summary.is_empty() {
        return Err("Anvil returned an empty pull request review summary".to_string());
    }

    let valid_paths = files
        .iter()
        .map(|file| file.filename.as_str())
        .collect::<BTreeSet<_>>();
    review.comments = review
        .comments
        .into_iter()
        .filter_map(|mut comment| {
            comment.path = comment.path.trim().to_string();
            comment.body = comment.body.trim().to_string();
            if comment.body.is_empty() || !valid_paths.contains(comment.path.as_str()) {
                return None;
            }
            Some(comment)
        })
        .collect();
    Ok(review)
}

fn build_review_prompt(
    repository: &RepoSlug,
    pull: &PullRequest,
    files: &[PullRequestFile],
) -> String {
    let mut prompt = format!(
        "You are Valkyrie, a GitHub pull request reviewer.\n\
         Review the pull request below and return a concise review summary plus actionable line comments.\n\
         Only comment on real defects, regressions, security issues, missing tests, or maintainability problems.\n\
         Do not comment on style preferences unless they hide a real risk.\n\n\
         Repository: {repository}\n\
         Pull request: #{}\n\
         URL: {}\n\
         Title: {}\n\
         Author: {}\n\
         Head branch: {}\n\
         Head sha: {}\n\n\
         Description:\n{}\n\n\
         Changed files:\n",
        pull.number,
        pull.html_url,
        pull.title,
        pull.user.login,
        pull.head.branch,
        pull.head.sha,
        pull.body.as_deref().unwrap_or("")
    );

    for file in files {
        prompt.push_str(&format!(
            "\n---\nPath: {}\nStatus: {}\nAdditions: {}\nDeletions: {}\nPatch:\n{}\n",
            file.filename,
            file.status,
            file.additions,
            file.deletions,
            truncate_patch(file.patch.as_deref().unwrap_or(""))
        ));
    }
    prompt
}

fn truncate_patch(patch: &str) -> String {
    const LIMIT: usize = 12_000;
    if patch.len() <= LIMIT {
        return patch.to_string();
    }
    let mut truncated = patch[..LIMIT].to_string();
    truncated.push_str("\n[patch truncated]\n");
    truncated
}

struct AnvilOptions {
    binary: String,
    default_model: Option<String>,
    max_turns: u16,
    show_logs: bool,
}

fn anvil_review(options: &AnvilOptions, prompt: &str) -> Result<GeneratedReview, String> {
    let cwd =
        std::env::current_dir().map_err(|error| format!("cannot read current dir: {error}"))?;
    let mut client = AcpClient::start(options)?;
    client.initialize()?;
    let session_id = client.new_session(&cwd)?;
    client.set_config(&session_id, "permission_mode", "readOnly")?;
    let response = client.prompt(&session_id, prompt)?;
    let output = extract_structured_output(&response)?;
    serde_json::from_value(output)
        .map_err(|error| format!("Anvil returned invalid review JSON: {error}"))
}

fn extract_structured_output(response: &Value) -> Result<Value, String> {
    let structured = find_structured_output(response)
        .ok_or_else(|| "Anvil response did not include structured output".to_string())?;
    let status = structured
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| "Anvil structured output did not include a status".to_string())?;
    match status {
        "success" | "coerced_success" => structured
            .get("validated_output")
            .cloned()
            .ok_or_else(|| "Anvil structured output did not include validated_output".to_string()),
        "validation_error" => Err(format!(
            "Anvil could not validate review output: {}",
            structured
                .get("invalid_excerpt")
                .and_then(Value::as_str)
                .unwrap_or("no excerpt")
        )),
        other => Err(format!(
            "Anvil returned unknown structured output status `{other}`"
        )),
    }
}

fn find_structured_output(value: &Value) -> Option<&Value> {
    if let Some(found) = value
        .get("anvil")
        .and_then(|anvil| anvil.get("structuredOutput"))
    {
        return Some(found);
    }
    match value {
        Value::Array(items) => items.iter().find_map(find_structured_output),
        Value::Object(map) => map.values().find_map(find_structured_output),
        _ => None,
    }
}

struct AcpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl AcpClient {
    fn start(options: &AnvilOptions) -> Result<Self, String> {
        let mut command = Command::new(&options.binary);
        command
            .arg("--max-turns")
            .arg(options.max_turns.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if options.show_logs {
                Stdio::inherit()
            } else {
                Stdio::null()
            });
        if let Some(default_model) = options.default_model.as_deref() {
            command.arg("--default-model").arg(default_model);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("cannot start Anvil `{}`: {error}", options.binary))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "cannot open Anvil stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "cannot open Anvil stdout".to_string())?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 0,
        })
    }

    fn initialize(&mut self) -> Result<(), String> {
        let params = json!({
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": { "readTextFile": true, "writeTextFile": false },
                "terminal": false
            },
            "clientInfo": {
                "name": "valkyrie",
                "title": "Valkyrie",
                "version": env!("CARGO_PKG_VERSION")
            }
        });
        self.request("initialize", params).map(|_| ())
    }

    fn new_session(&mut self, cwd: &Path) -> Result<String, String> {
        let result = self.request(
            "session/new",
            json!({
                "cwd": cwd.to_string_lossy(),
                "mcpServers": []
            }),
        )?;
        result
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "Anvil session/new response did not include sessionId".to_string())
    }

    fn set_config(&mut self, session_id: &str, config_id: &str, value: &str) -> Result<(), String> {
        self.request(
            "session/set_config_option",
            json!({
                "sessionId": session_id,
                "configId": config_id,
                "value": value
            }),
        )
        .map(|_| ())
    }

    fn prompt(&mut self, session_id: &str, prompt: &str) -> Result<Value, String> {
        self.request(
            "session/prompt",
            json!({
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": prompt }],
                "_meta": {
                    "anvil": {
                        "structuredOutput": {
                            "schemaName": "valkyrie_pr_review",
                            "allowCoercion": true,
                            "schema": review_schema()
                        }
                    }
                }
            }),
        )
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        writeln!(self.stdin, "{request}")
            .map_err(|error| format!("cannot write ACP request `{method}`: {error}"))?;
        self.stdin
            .flush()
            .map_err(|error| format!("cannot flush ACP request `{method}`: {error}"))?;
        self.read_response(id)
    }

    fn read_response(&mut self, id: u64) -> Result<Value, String> {
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = self
                .stdout
                .read_line(&mut line)
                .map_err(|error| format!("cannot read Anvil response: {error}"))?;
            if bytes == 0 {
                return Err("Anvil exited before returning an ACP response".to_string());
            }
            let message: Value = serde_json::from_str(line.trim())
                .map_err(|error| format!("Anvil returned invalid JSON-RPC: {error}: {line}"))?;
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(format!("Anvil ACP request failed: {error}"));
            }
            return message
                .get("result")
                .cloned()
                .ok_or_else(|| "Anvil ACP response did not include result".to_string());
        }
    }
}

impl Drop for AcpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn review_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["summary", "comments"],
        "properties": {
            "summary": {
                "type": "string",
                "description": "A concise pull request review summary in Markdown."
            },
            "comments": {
                "type": "array",
                "description": "Actionable inline review comments. Use an empty array if there are no inline findings.",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["path", "line", "body"],
                    "properties": {
                        "path": { "type": "string" },
                        "line": { "type": ["integer", "null"], "minimum": 1 },
                        "body": { "type": "string" }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repository_slug() {
        let slug: RepoSlug = "BrokkAi/valkyrie".parse().expect("valid slug");
        assert_eq!(slug.owner, "BrokkAi");
        assert_eq!(slug.name, "valkyrie");
    }

    #[test]
    fn rejects_invalid_repository_slug() {
        assert!("BrokkAi".parse::<RepoSlug>().is_err());
        assert!("BrokkAi/val kyrie".parse::<RepoSlug>().is_err());
    }

    #[test]
    fn merges_and_deduplicates_repositories() {
        let one: RepoSlug = "BrokkAi/one".parse().expect("slug");
        let two: RepoSlug = "BrokkAi/two".parse().expect("slug");
        let merged = merge_repositories(vec![one.clone()], vec![one.clone(), two.clone()])
            .expect("merged repositories");
        assert_eq!(merged, vec![one, two]);
    }

    #[test]
    fn builds_stable_review_state_key() {
        let repo: RepoSlug = "BrokkAi/valkyrie".parse().expect("slug");
        assert_eq!(review_state_key(&repo, 42), "BrokkAi/valkyrie#42");
    }

    #[test]
    fn formats_repository_access_error_with_context() {
        let repo: RepoSlug = "BrokkAi/valkryrie".parse().expect("slug");
        let error = repository_access_error(&repo, "gh: Not Found (HTTP 404)");
        assert!(error.contains("cannot access repository `BrokkAi/valkryrie`"));
        assert!(error.contains("gh: Not Found (HTTP 404)"));
        assert!(error.contains("repository exists"));
        assert!(error.contains("authenticated GitHub user can read it"));
    }

    #[test]
    fn filters_invalid_generated_comments() {
        let files = vec![PullRequestFile {
            filename: "src/lib.rs".to_string(),
            status: "modified".to_string(),
            additions: 1,
            deletions: 1,
            patch: None,
        }];
        let review = GeneratedReview {
            summary: "Looks good except one issue.".to_string(),
            comments: vec![
                GeneratedComment {
                    path: "src/lib.rs".to_string(),
                    line: Some(10),
                    body: "Fix this.".to_string(),
                },
                GeneratedComment {
                    path: "missing.rs".to_string(),
                    line: Some(1),
                    body: "Nope.".to_string(),
                },
            ],
        };
        let sanitized = sanitize_generated_review(review, &files).expect("sanitized review");
        assert_eq!(sanitized.comments.len(), 1);
        assert_eq!(sanitized.comments[0].path, "src/lib.rs");
    }

    #[test]
    fn extracts_successful_structured_output() {
        let value = json!({
            "meta": {
                "anvil": {
                    "structuredOutput": {
                        "status": "success",
                        "schema_name": "valkyrie_pr_review",
                        "validated_output": {
                            "summary": "Summary",
                            "comments": []
                        },
                        "coercion_requested": false
                    }
                }
            }
        });
        let output = extract_structured_output(&value).expect("structured output");
        assert_eq!(output["summary"], "Summary");
    }

    #[test]
    fn finds_structured_output_in_nested_anvil_metadata() {
        let value = json!({
            "result": {
                "_meta": {
                    "anvil": {
                        "structuredOutput": {
                            "status": "success",
                            "schema_name": "valkyrie_pr_review",
                            "validated_output": {
                                "summary": "Nested",
                                "comments": []
                            },
                            "coercion_requested": false
                        }
                    }
                }
            }
        });
        let output = extract_structured_output(&value).expect("structured output");
        assert_eq!(output["summary"], "Nested");
    }
}
