//! Read-only GitHub context for the assistant (overhaul piece M).
//!
//! The tools read a snapshot of the repositories the user explicitly selected. Rust fetches that
//! snapshot with GET requests only (`fetch_context`); the token is a parameter of that one function
//! and is never stored in the snapshot, so it can't reach a tool result, a trace, or Python. Tool
//! calls never touch the network, which keeps them fast, offline-safe, and inside the call budget.
//!
//! Everything GitHub returns is untrusted: anyone who can open an issue or comment on a public
//! repository can write text that reaches the model. Every GitHub record therefore carries
//! `trust: "untrusted"`, its text is cleaned (`sanitize`), and it lists `injectionSignals`. The
//! signals are advisory and easy to evade by rephrasing; the defence that counts is the agent
//! treating tool data as data, which the benchmark's `untrusted_input` cases measure. The benchmark
//! mock (`agent_bench/github_tools.py`) mirrors this module, and both must reproduce the shared
//! vectors in `agent_bench/fixtures/github-sanitize.v1.json`.

use super::error::{ToolError, ToolErrorCode};
use crate::error::{AppError, AppResult};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};

pub const MAX_TITLE_CHARS: usize = 256;
pub const MAX_BODY_CHARS: usize = 4_000;
pub const MAX_COMMENT_CHARS: usize = 1_000;
pub const MAX_COMMENTS: usize = 5;
pub const MAX_LABEL_CHARS: usize = 50;
pub const MAX_LABELS: usize = 10;
pub const MAX_REPO_DESCRIPTION_CHARS: usize = 300;
pub const MAX_BRANCH_CHARS: usize = 100;
pub const MAX_QUERY_CHARS: usize = 200;
pub const MAX_SEARCH_LIMIT: u32 = 20;
pub const DEFAULT_SEARCH_LIMIT: u32 = 10;
/// The most repositories a user can share at once.
pub const MAX_SELECTED_REPOS: usize = 10;
/// How many recently updated issues and pull requests a sync keeps per repository.
pub const ITEMS_PER_REPO: usize = 50;
/// The JSON Schema pattern for `owner/name`.
pub const REPO_PATTERN: &str = "^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$";

// ------------------------------------------------------------------------------------------------
// The snapshot

/// What the user shared and the last synced copy of it. `repos` and `items` may hold more than
/// `selected_repos` (a repository the user just unselected, for example); the tools only ever
/// return selected ones.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubContext {
    #[serde(default)]
    pub selected_repos: Vec<String>,
    #[serde(default)]
    pub synced_at: Option<String>,
    /// Whether issue and pull request bodies and comments may be shown to the agent.
    #[serde(default)]
    pub include_bodies: bool,
    #[serde(default)]
    pub repos: Vec<GitHubRepo>,
    #[serde(default)]
    pub items: Vec<GitHubItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubRepo {
    pub full_name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
    #[serde(default)]
    pub private: bool,
    /// GitHub's own count of open issues plus open pull requests, when the sync read it.
    #[serde(default)]
    pub open_items_total: Option<u64>,
    /// Whether the sync kept only the most recently updated items, so counts taken from the
    /// snapshot may miss older open ones.
    #[serde(default)]
    pub items_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Issue,
    PullRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemState {
    Open,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubItem {
    pub repo: String,
    pub number: u64,
    pub kind: ItemKind,
    pub state: ItemState,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub assignees: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub closed_at: Option<String>,
    #[serde(default)]
    pub merged_at: Option<String>,
    #[serde(default)]
    pub draft: bool,
    /// How many comments the item has on GitHub; `comments` may hold fewer.
    #[serde(default)]
    pub comment_total: Option<u64>,
    #[serde(default)]
    pub comments: Vec<GitHubComment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHubComment {
    #[serde(default)]
    pub author: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub body: Option<String>,
}

impl GitHubContext {
    /// Whether the user has shared at least one repository.
    pub fn connected(&self) -> bool {
        !self.selected_repos.is_empty()
    }

    fn selected(&self) -> BTreeSet<String> {
        self.selected_repos
            .iter()
            .map(|repo| repo.to_ascii_lowercase())
            .collect()
    }

    fn require_repo(&self, repo: &str) -> Result<String, ToolError> {
        let key = repo.to_ascii_lowercase();
        if self.selected().contains(&key) {
            Ok(key)
        } else {
            Err(ToolError::new(
                ToolErrorCode::PermissionDenied,
                format!("{repo} is not one of the repositories the user selected."),
            ))
        }
    }

    /// Items from `repos`, newest activity first.
    fn visible_items(&self, repos: &BTreeSet<String>) -> Vec<&GitHubItem> {
        let mut items: Vec<&GitHubItem> = self
            .items
            .iter()
            .filter(|item| repos.contains(&item.repo.to_ascii_lowercase()))
            .collect();
        items.sort_by(|a, b| {
            (&b.updated_at, b.repo.to_ascii_lowercase(), b.number).cmp(&(
                &a.updated_at,
                a.repo.to_ascii_lowercase(),
                a.number,
            ))
        });
        items
    }
}

/// Whether `repo` looks like `owner/name` (so it is safe to put in an API path).
pub fn valid_repo_name(repo: &str) -> bool {
    let allowed = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    };
    match repo.split_once('/') {
        Some((owner, name)) => allowed(owner) && allowed(name) && repo.len() >= 3,
        None => false,
    }
}

// ------------------------------------------------------------------------------------------------
// Cleaning untrusted text

/// The advisory signals, in the order they are reported.
pub const SIGNALS: [&str; 7] = [
    "hidden_comment",
    "hidden_characters",
    "instruction_override",
    "role_marker",
    "action_request",
    "secret_request",
    "concealment",
];

/// Phrases behind each wording signal, matched against the ASCII-lowercased cleaned text.
const SIGNAL_PHRASES: [(&str, &[&str]); 5] = [
    (
        "instruction_override",
        &[
            "ignore previous",
            "ignore all previous",
            "ignore the above",
            "ignore prior",
            "ignore your instructions",
            "disregard previous",
            "disregard all",
            "disregard the above",
            "disregard your instructions",
            "forget your instructions",
            "forget previous",
            "new instructions",
            "override your",
            "instead of answering",
        ],
    ),
    (
        "role_marker",
        &[
            "system:",
            "assistant:",
            "<|",
            "|>",
            "[inst]",
            "### instruction",
            "you are now",
            "system prompt",
            "developer message",
            "</s>",
        ],
    ),
    (
        "action_request",
        &[
            "create_",
            "update_",
            "delete_",
            "reschedule_",
            "schedule_task",
            "approve all",
            "approve the",
            "apply the proposal",
            "apply all",
            "delete all",
            "delete the",
            "cancel all",
            "mark all",
            "send the",
            "email the",
            "forward the",
            "post the",
            "run the command",
            "execute the",
        ],
    ),
    (
        "secret_request",
        &[
            "token",
            "password",
            "api key",
            "apikey",
            "secret",
            "credential",
            "oauth",
        ],
    ),
    (
        "concealment",
        &[
            "do not tell the user",
            "don't tell the user",
            "without telling",
            "do not mention",
            "don't mention",
            "keep this secret",
            "silently",
        ],
    ),
];

/// One piece of GitHub text after cleaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanText {
    pub text: String,
    pub truncated: bool,
    pub signals: BTreeSet<&'static str>,
}

/// Characters that render as nothing (or reorder text) and are used to hide instructions from a
/// human reader while the model still reads them. U+E0000..U+E007F are "tag" characters, which
/// can spell out a whole hidden ASCII message.
fn invisible(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x2069
            | 0xFEFF
            | 0xE0000..=0xE007F
    )
}

fn strip_invisible(text: &str, signals: &mut BTreeSet<&'static str>) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let code = c as u32;
        if invisible(c) {
            signals.insert("hidden_characters");
        } else if c == '\n' || c == '\t' {
            out.push(c);
        } else if code < 0x20 || (0x7F..=0x9F).contains(&code) {
            continue;
        } else {
            out.push(c);
        }
    }
    out
}

/// Removes HTML comments, which GitHub doesn't render but a model reads in full. An unclosed
/// comment hides everything after it, so it is removed to the end.
fn strip_comments(text: &str, signals: &mut BTreeSet<&'static str>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        let (hidden, next) = match after.find("-->") {
            Some(end) => (&after[..end], Some(&after[end + 3..])),
            None => (after, None),
        };
        if !hidden.trim().is_empty() {
            signals.insert("hidden_comment");
        }
        match next {
            Some(next) => rest = next,
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

fn url_at(chars: &[char], i: usize) -> bool {
    ["http://", "https://"].iter().any(|prefix| {
        let n = prefix.len();
        i + n <= chars.len()
            && chars[i..i + n]
                .iter()
                .zip(prefix.chars())
                .all(|(c, p)| c.to_ascii_lowercase() == p)
    })
}

fn link_host(url: &str) -> String {
    let rest = url.split_once("://").map_or("", |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = host_port.split(':').next().unwrap_or("");
    host.to_ascii_lowercase()
}

/// Replaces every link with the host it points to. The agent has no browser, and a URL with a
/// query string is the usual template for leaking data ("…?q=<the user's calendar>").
fn replace_links(text: &str) -> String {
    const STOP: &str = " \t\n<>\"'()[]{}`";
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if url_at(&chars, i) {
            let mut j = i;
            while j < chars.len() && !STOP.contains(chars[j]) {
                j += 1;
            }
            let url: String = chars[i..j].iter().collect();
            let host = link_host(&url);
            if host.is_empty() {
                out.push_str("[link]");
            } else {
                out.push_str(&format!("[link: {host}]"));
            }
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Cleans one piece of GitHub text: drops invisible characters and HTML comments, replaces links
/// with their host, breaks up chat-template control tokens, trims, and truncates to `max_chars`
/// characters. Signals are found before truncation, so an instruction past the cut still counts.
pub fn sanitize(text: Option<&str>, max_chars: usize) -> CleanText {
    let mut signals = BTreeSet::new();
    let value = strip_invisible(text.unwrap_or(""), &mut signals);
    let value = strip_comments(&value, &mut signals);
    let value = replace_links(&value);
    let lowered = value.to_ascii_lowercase();
    for (signal, phrases) in SIGNAL_PHRASES {
        if phrases.iter().any(|phrase| lowered.contains(phrase)) {
            signals.insert(signal);
        }
    }
    let value = value.replace("<|", "< |").replace("|>", "| >");
    let value = value.trim();
    let truncated = value.chars().count() > max_chars;
    let text = if truncated {
        let cut: String = value.chars().take(max_chars).collect();
        format!("{}…", cut.trim_end())
    } else {
        value.to_string()
    };
    CleanText {
        text,
        truncated,
        signals,
    }
}

/// Signals in their reporting order.
fn ordered(signals: &BTreeSet<&'static str>) -> Vec<&'static str> {
    SIGNALS
        .into_iter()
        .filter(|signal| signals.contains(signal))
        .collect()
}

// ------------------------------------------------------------------------------------------------
// Tool inputs and outputs

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListReposInput {}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchItemsInput {
    pub repo: Option<String>,
    pub query: Option<String>,
    pub kind: Option<ItemKind>,
    pub state: Option<ItemState>,
    pub label: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetItemInput {
    pub repo: String,
    pub number: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoView {
    pub id: String,
    pub full_name: String,
    pub description: String,
    pub default_branch: Option<String>,
    pub private: bool,
    /// Open issues and pull requests counted in the synced snapshot.
    pub open_issues: usize,
    pub open_pull_requests: usize,
    /// False when the snapshot kept only recent items, so the two counts above may be low.
    pub counts_complete: bool,
    /// GitHub's total of open issues plus open pull requests, when known.
    pub open_items_total: Option<u64>,
    pub trust: &'static str,
    pub injection_signals: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListReposOutput {
    pub synced_at: Option<String>,
    pub repos: Vec<RepoView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemSummary {
    pub id: String,
    pub repo: String,
    pub number: u64,
    pub kind: ItemKind,
    pub state: ItemState,
    pub merged: bool,
    pub draft: bool,
    pub title: String,
    pub author: Option<String>,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub comment_count: u64,
    pub trust: &'static str,
    pub injection_signals: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchItemsOutput {
    pub synced_at: Option<String>,
    pub items: Vec<ItemSummary>,
    pub total_matches: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentView {
    pub author: Option<String>,
    pub created_at: String,
    pub body: String,
    pub truncated: bool,
    pub injection_signals: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemDetail {
    #[serde(flatten)]
    pub summary: ItemSummary,
    pub bodies_included: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments: Option<Vec<CommentView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments_truncated: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetItemOutput {
    pub synced_at: Option<String>,
    pub item: ItemDetail,
}

fn summarize(item: &GitHubItem) -> ItemSummary {
    let title = sanitize(Some(&item.title), MAX_TITLE_CHARS);
    let mut signals = title.signals;
    let mut labels = Vec::new();
    for raw in item.labels.iter().take(MAX_LABELS) {
        let clean = sanitize(Some(raw), MAX_LABEL_CHARS);
        if !clean.text.is_empty() {
            labels.push(clean.text);
        }
        signals.extend(clean.signals);
    }
    ItemSummary {
        id: format!("{}#{}", item.repo, item.number),
        repo: item.repo.clone(),
        number: item.number,
        kind: item.kind,
        state: item.state,
        merged: item.merged_at.is_some(),
        draft: item.draft,
        title: title.text,
        author: item.author.clone(),
        labels,
        assignees: item.assignees.clone(),
        created_at: item.created_at.clone(),
        updated_at: item.updated_at.clone(),
        closed_at: item.closed_at.clone(),
        comment_count: item
            .comment_total
            .unwrap_or(item.comments.len() as u64)
            .max(item.comments.len() as u64),
        trust: "untrusted",
        injection_signals: ordered(&signals),
    }
}

// ------------------------------------------------------------------------------------------------
// The tools

pub fn list_repos(context: &GitHubContext, _input: ListReposInput) -> ListReposOutput {
    let selected = context.selected();
    let items = context.visible_items(&selected);
    let mut repos: Vec<&GitHubRepo> = context
        .repos
        .iter()
        .filter(|repo| selected.contains(&repo.full_name.to_ascii_lowercase()))
        .collect();
    repos.sort_by_key(|repo| repo.full_name.to_ascii_lowercase());
    let repos = repos
        .into_iter()
        .map(|repo| {
            let key = repo.full_name.to_ascii_lowercase();
            let open = |kind: ItemKind| {
                items
                    .iter()
                    .filter(|item| {
                        item.repo.to_ascii_lowercase() == key
                            && item.state == ItemState::Open
                            && item.kind == kind
                    })
                    .count()
            };
            let description = sanitize(repo.description.as_deref(), MAX_REPO_DESCRIPTION_CHARS);
            let mut signals = description.signals;
            // Branch names are free text too (`main<|im_start|>system` is a valid name).
            let default_branch = repo.default_branch.as_deref().map(|branch| {
                let clean = sanitize(Some(branch), MAX_BRANCH_CHARS);
                signals.extend(clean.signals);
                clean.text
            });
            RepoView {
                id: repo.full_name.clone(),
                full_name: repo.full_name.clone(),
                description: description.text,
                default_branch,
                private: repo.private,
                open_issues: open(ItemKind::Issue),
                open_pull_requests: open(ItemKind::PullRequest),
                counts_complete: !repo.items_truncated,
                open_items_total: repo.open_items_total,
                trust: "untrusted",
                injection_signals: ordered(&signals),
            }
        })
        .collect();
    ListReposOutput {
        synced_at: context.synced_at.clone(),
        repos,
    }
}

pub fn search_items(
    context: &GitHubContext,
    input: SearchItemsInput,
) -> Result<SearchItemsOutput, ToolError> {
    let limit = input.limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
    if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
        return Err(ToolError::invalid(format!(
            "limit must be between 1 and {MAX_SEARCH_LIMIT}."
        )));
    }
    let query = input.query.unwrap_or_default();
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(ToolError::invalid(format!(
            "query is limited to {MAX_QUERY_CHARS} characters."
        )));
    }
    if input
        .label
        .as_ref()
        .is_some_and(|label| label.chars().count() > MAX_LABEL_CHARS)
    {
        return Err(ToolError::invalid(format!(
            "label is limited to {MAX_LABEL_CHARS} characters."
        )));
    }
    let repos = match &input.repo {
        Some(repo) => {
            if !valid_repo_name(repo) {
                return Err(ToolError::invalid("repo must look like owner/name."));
            }
            BTreeSet::from([context.require_repo(repo)?])
        }
        None => context.selected(),
    };
    let words: Vec<String> = query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let label = input.label.map(|label| label.to_lowercase());
    let mut hits = Vec::new();
    for item in context.visible_items(&repos) {
        if input.kind.is_some_and(|kind| item.kind != kind)
            || input.state.is_some_and(|state| item.state != state)
        {
            continue;
        }
        let summary = summarize(item);
        if let Some(label) = &label {
            if !summary.labels.iter().any(|l| &l.to_lowercase() == label) {
                continue;
            }
        }
        let haystack = std::iter::once(summary.title.as_str())
            .chain(summary.labels.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if !words.iter().all(|word| haystack.contains(word.as_str())) {
            continue;
        }
        hits.push(summary);
    }
    let total_matches = hits.len();
    hits.truncate(limit as usize);
    Ok(SearchItemsOutput {
        synced_at: context.synced_at.clone(),
        items: hits,
        total_matches,
        truncated: total_matches > limit as usize,
    })
}

pub fn get_item(context: &GitHubContext, input: GetItemInput) -> Result<GetItemOutput, ToolError> {
    if !valid_repo_name(&input.repo) {
        return Err(ToolError::invalid("repo must look like owner/name."));
    }
    if input.number == 0 {
        return Err(ToolError::invalid("number must be 1 or more."));
    }
    let key = context.require_repo(&input.repo)?;
    let item = context
        .items
        .iter()
        .find(|item| item.repo.to_ascii_lowercase() == key && item.number == input.number)
        .ok_or_else(|| {
            ToolError::not_found(format!(
                "{}#{} is not in the synced GitHub context.",
                input.repo, input.number
            ))
        })?;
    let mut summary = summarize(item);
    let mut detail = ItemDetail {
        bodies_included: context.include_bodies,
        body: None,
        body_truncated: None,
        comments: None,
        comments_truncated: None,
        summary: summary.clone(),
    };
    if context.include_bodies {
        let mut signals: BTreeSet<&'static str> =
            summary.injection_signals.iter().copied().collect();
        let body = sanitize(item.body.as_deref(), MAX_BODY_CHARS);
        signals.extend(body.signals.iter().copied());
        let mut comments: Vec<&GitHubComment> = item.comments.iter().collect();
        comments.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let skip = comments.len().saturating_sub(MAX_COMMENTS);
        let views = comments
            .into_iter()
            .skip(skip)
            .map(|comment| {
                let clean = sanitize(comment.body.as_deref(), MAX_COMMENT_CHARS);
                signals.extend(clean.signals.iter().copied());
                CommentView {
                    author: comment.author.clone(),
                    created_at: comment.created_at.clone(),
                    body: clean.text,
                    truncated: clean.truncated,
                    injection_signals: ordered(&clean.signals),
                }
            })
            .collect();
        summary.injection_signals = ordered(&signals);
        detail.summary = summary;
        detail.body = Some(body.text);
        detail.body_truncated = Some(body.truncated);
        detail.comments = Some(views);
        let shown = detail.comments.as_ref().map_or(0, Vec::len) as u64;
        detail.comments_truncated = Some(detail.summary.comment_count > shown);
    }
    Ok(GetItemOutput {
        synced_at: context.synced_at.clone(),
        item: detail,
    })
}

// ------------------------------------------------------------------------------------------------
// Syncing from GitHub (GET only)

/// The public GitHub REST API.
pub const GITHUB_API: &str = "https://api.github.com";

#[derive(Deserialize)]
struct RestUser {
    login: String,
}

#[derive(Deserialize)]
struct RestLabel {
    name: String,
}

#[derive(Deserialize)]
struct RestRepo {
    full_name: String,
    description: Option<String>,
    default_branch: Option<String>,
    #[serde(default)]
    private: bool,
    open_issues_count: Option<u64>,
}

#[derive(Deserialize)]
struct RestPullRef {
    merged_at: Option<String>,
}

#[derive(Deserialize)]
struct RestIssue {
    number: u64,
    title: String,
    body: Option<String>,
    state: String,
    user: Option<RestUser>,
    #[serde(default)]
    labels: Vec<RestLabel>,
    #[serde(default)]
    assignees: Vec<RestUser>,
    created_at: String,
    updated_at: String,
    closed_at: Option<String>,
    #[serde(default)]
    draft: Option<bool>,
    #[serde(default)]
    comments: Option<u64>,
    pull_request: Option<RestPullRef>,
}

#[derive(Deserialize)]
struct RestComment {
    issue_url: String,
    user: Option<RestUser>,
    created_at: String,
    body: Option<String>,
}

/// Downloads a fresh snapshot of `selected` repositories. Every request is a GET; the token (if
/// any) is sent only in the Authorization header and is not kept anywhere in the result.
pub async fn fetch_context(
    client: &reqwest::Client,
    api_base: &str,
    token: Option<&str>,
    selected: &[String],
    include_bodies: bool,
    synced_at: DateTime<Utc>,
) -> AppResult<GitHubContext> {
    if selected.len() > MAX_SELECTED_REPOS {
        return Err(AppError::Validation(format!(
            "Share at most {MAX_SELECTED_REPOS} repositories with the assistant."
        )));
    }
    let base = api_base.trim_end_matches('/');
    let mut context = GitHubContext {
        selected_repos: selected.to_vec(),
        synced_at: Some(synced_at.to_rfc3339_opts(SecondsFormat::Secs, true)),
        include_bodies,
        ..GitHubContext::default()
    };
    for repo in selected {
        if !valid_repo_name(repo) {
            return Err(AppError::Validation(format!(
                "'{repo}' is not a repository name like owner/name."
            )));
        }
        let meta: RestRepo = get_json(client, &format!("{base}/repos/{repo}"), token, repo).await?;
        let issues: Vec<RestIssue> = get_json(
            client,
            &format!(
                "{base}/repos/{repo}/issues?state=all&sort=updated&direction=desc&per_page={ITEMS_PER_REPO}"
            ),
            token,
            repo,
        )
        .await?;
        let mut comments: HashMap<u64, Vec<GitHubComment>> = HashMap::new();
        if include_bodies {
            let rest: Vec<RestComment> = get_json(
                client,
                &format!(
                    "{base}/repos/{repo}/issues/comments?sort=created&direction=desc&per_page=100"
                ),
                token,
                repo,
            )
            .await?;
            for comment in rest {
                let Some(number) = comment
                    .issue_url
                    .rsplit('/')
                    .next()
                    .and_then(|n| n.parse().ok())
                else {
                    continue;
                };
                comments.entry(number).or_default().push(GitHubComment {
                    author: comment.user.map(|user| user.login),
                    created_at: comment.created_at,
                    body: comment.body,
                });
            }
        }
        context.repos.push(GitHubRepo {
            full_name: meta.full_name,
            description: meta.description,
            default_branch: meta.default_branch,
            private: meta.private,
            open_items_total: meta.open_issues_count,
            items_truncated: issues.len() >= ITEMS_PER_REPO,
        });
        for issue in issues {
            let comments = comments.remove(&issue.number).unwrap_or_default();
            context.items.push(GitHubItem {
                repo: repo.clone(),
                number: issue.number,
                kind: if issue.pull_request.is_some() {
                    ItemKind::PullRequest
                } else {
                    ItemKind::Issue
                },
                state: if issue.state == "open" {
                    ItemState::Open
                } else {
                    ItemState::Closed
                },
                title: issue.title,
                body: include_bodies.then_some(issue.body).flatten(),
                author: issue.user.map(|user| user.login),
                labels: issue.labels.into_iter().map(|label| label.name).collect(),
                assignees: issue.assignees.into_iter().map(|user| user.login).collect(),
                created_at: issue.created_at,
                updated_at: issue.updated_at,
                closed_at: issue.closed_at,
                merged_at: issue.pull_request.and_then(|pull| pull.merged_at),
                draft: issue.draft.unwrap_or(false),
                comment_total: issue.comments,
                comments,
            });
        }
    }
    Ok(context)
}

async fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
    repo: &str,
) -> AppResult<T> {
    let mut request = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "DelvePlanner");
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?;
    match response.status().as_u16() {
        200..=299 => Ok(response.json().await?),
        401 | 403 => Err(AppError::Validation(format!(
            "GitHub refused access to {repo}. Check the token's read permissions."
        ))),
        404 => Err(AppError::Validation(format!(
            "{repo} wasn't found, or this token can't see it."
        ))),
        status => Err(AppError::Validation(format!(
            "GitHub answered {status} for {repo}. Try again later."
        ))),
    }
}

#[cfg(test)]
mod tests;
