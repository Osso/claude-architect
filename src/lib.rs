use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub enum Request {
    Validate {
        project: String,
        goal: String,
        tasks: Vec<String>,
        /// Working directory of the calling project.
        cwd: String,
    },
    /// Regenerate design doc and reset session for a project.
    Reset { project: String, cwd: String },
    /// Brief completion report from a finished task (assessed by Haiku).
    Report {
        project: String,
        task_description: String,
        assessment: String,
        cwd: String,
    },
    Ping,
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub enum Response {
    Verdict(String),
    Pong,
    Error(String),
}

pub fn socket_path() -> String {
    let dir =
        std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR must be set");
    format!("{dir}/claude-architect.sock")
}

/// Subagent types that are read-only — skip architect validation.
pub const EXPLORATION_AGENTS: &[&str] = &[
    "Explore",
    "Plan",
    "feature-dev:code-explorer",
    "feature-dev:code-reviewer",
    "feature-dev:code-architect",
    "architect",
    "claude-code-guide",
    "statusline-setup",
];

/// Returns true if this subagent type should skip architect validation.
pub fn should_skip(subagent_type: &str) -> bool {
    if subagent_type.is_empty() {
        return false;
    }
    EXPLORATION_AGENTS
        .iter()
        .any(|&a| a.eq_ignore_ascii_case(subagent_type))
}

/// Truncate a string to `max` bytes on a char boundary, appending "...".
pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let end = s.floor_char_boundary(max);
        format!("{}...", &s[..end])
    }
}

/// Check if an architect verdict contains "needs-changes".
pub fn contains_needs_changes(verdict: &str) -> bool {
    verdict
        .lines()
        .any(|line| line.contains("VERDICT:") && line.contains("needs-changes"))
}

/// Build the deny JSON for a PreToolUse hook response.
pub fn deny_json(reason: &str) -> String {
    let escaped = reason.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        r#"{{"hookSpecificOutput":{{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Architect: {escaped}"}}}}"#
    )
}

/// Build the prompt for Haiku to assess whether a task was accomplished.
pub fn build_assessment_prompt(description: &str, result: &str) -> String {
    format!(
        "Assess whether this task was accomplished.\n\n\
         ## Task\n{description}\n\n\
         ## Output\n{result}\n\n\
         Respond with exactly one line:\n\
         ACCOMPLISHED - if the task was completed successfully\n\
         INCOMPLETE: <brief reason> - if not completed or only partially done"
    )
}

/// Build the PostToolUse response JSON with additionalContext feedback.
pub fn feedback_json(feedback: &str) -> String {
    let escaped = feedback.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        r#"{{"hookSpecificOutput":{{"hookEventName":"PostToolUse","additionalContext":"Architect: {escaped}"}}}}"#
    )
}

/// Check if a Haiku assessment indicates the task was incomplete.
pub fn contains_incomplete(assessment: &str) -> bool {
    assessment
        .lines()
        .any(|line| line.starts_with("INCOMPLETE"))
}

/// Build the validation prompt sent to claude.
pub fn build_validation_prompt(goal: &str, tasks: &[String]) -> String {
    let mut prompt = String::new();
    prompt.push_str("VALIDATE this task decomposition. Do NOT solve, implement, or execute the tasks. Only check for conflicts, gaps, ordering, and scope issues. Return VERDICT + structured assessment.\n\n");
    prompt.push_str("## Goal\n\n");
    prompt.push_str(goal);
    prompt.push_str("\n\n## Tasks to Validate\n\n");
    for (i, task) in tasks.iter().enumerate() {
        prompt.push_str(&format!("{}. {}\n", i + 1, task));
    }
    prompt
}

/// Strip YAML frontmatter (---...---) from markdown content.
pub fn strip_frontmatter(content: &str) -> &str {
    if content.starts_with("---") {
        if let Some(end) = content[3..].find("---") {
            return content[end + 6..].trim_start();
        }
    }
    content
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Request/Response serde roundtrip ---

    #[test]
    fn roundtrip_ping() {
        let req = Request::Ping;
        let bytes = rmp_serde::to_vec(&req).unwrap();
        let decoded: Request = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(decoded, Request::Ping);
    }

    #[test]
    fn roundtrip_validate() {
        let req = Request::Validate {
            project: "test-project".into(),
            goal: "deploy feature".into(),
            tasks: vec!["write code".into(), "run tests".into()],
            cwd: "/home/user/projects/test".into(),
        };
        let bytes = rmp_serde::to_vec(&req).unwrap();
        let decoded: Request = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn roundtrip_report() {
        let req = Request::Report {
            project: "myproject".into(),
            task_description: "fix auth bug".into(),
            assessment: "ACCOMPLISHED".into(),
            cwd: "/home/user/projects/test".into(),
        };
        let bytes = rmp_serde::to_vec(&req).unwrap();
        let decoded: Request = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn roundtrip_response_verdict() {
        let resp = Response::Verdict("VERDICT: ok\n\n1. [ok] task".into());
        let bytes = rmp_serde::to_vec(&resp).unwrap();
        let decoded: Response = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(decoded, resp);
    }

    #[test]
    fn roundtrip_response_error() {
        let resp = Response::Error("something failed".into());
        let bytes = rmp_serde::to_vec(&resp).unwrap();
        let decoded: Response = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(decoded, resp);
    }

    // --- should_skip ---

    #[test]
    fn skip_exploration_agents() {
        assert!(should_skip("Explore"));
        assert!(should_skip("Plan"));
        assert!(should_skip("architect"));
        assert!(should_skip("feature-dev:code-explorer"));
        assert!(should_skip("claude-code-guide"));
        assert!(should_skip("statusline-setup"));
    }

    #[test]
    fn skip_is_case_insensitive() {
        assert!(should_skip("explore"));
        assert!(should_skip("PLAN"));
        assert!(should_skip("ARCHITECT"));
    }

    #[test]
    fn do_not_skip_implementation_agents() {
        assert!(!should_skip("general-purpose"));
        assert!(!should_skip("Bash"));
        assert!(!should_skip("custom-worker"));
    }

    #[test]
    fn empty_subagent_type_not_skipped() {
        assert!(!should_skip(""));
    }

    // --- truncate ---

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_exact_length() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long_string() {
        let result = truncate("hello world", 5);
        assert_eq!(result, "hello...");
    }

    #[test]
    fn truncate_respects_char_boundary() {
        // "café" is 5 bytes (é is 2 bytes), truncating at 4 should land on 'f'
        let result = truncate("café", 4);
        assert_eq!(result, "caf...");
    }

    // --- contains_needs_changes ---

    #[test]
    fn detects_needs_changes() {
        assert!(contains_needs_changes("VERDICT: needs-changes\n\nstuff"));
    }

    #[test]
    fn detects_ok() {
        assert!(!contains_needs_changes("VERDICT: ok\n\n1. [ok] task"));
    }

    #[test]
    fn needs_changes_requires_verdict_prefix() {
        assert!(!contains_needs_changes(
            "this needs-changes but no VERDICT"
        ));
    }

    #[test]
    fn needs_changes_both_on_same_line() {
        assert!(contains_needs_changes(
            "some preamble\nVERDICT: needs-changes\nmore"
        ));
        assert!(!contains_needs_changes(
            "VERDICT: ok\nneeds-changes on next line"
        ));
    }

    // --- deny_json ---

    #[test]
    fn deny_json_basic() {
        let json = deny_json("bad plan");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let reason = parsed["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.contains("bad plan"));
        assert_eq!(
            parsed["hookSpecificOutput"]["permissionDecision"],
            "deny"
        );
    }

    #[test]
    fn deny_json_escapes_quotes() {
        let json = deny_json(r#"task "foo" conflicts"#);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let reason = parsed["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.contains(r#"task "foo" conflicts"#));
    }

    #[test]
    fn deny_json_escapes_backslashes() {
        let json = deny_json(r"path\to\file");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let reason = parsed["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.contains(r"path\to\file"));
    }

    // --- build_validation_prompt ---

    #[test]
    fn prompt_includes_goal_and_tasks() {
        let prompt = build_validation_prompt(
            "deploy service",
            &["write code".into(), "run tests".into()],
        );
        assert!(prompt.starts_with("VALIDATE this task decomposition."));
        assert!(prompt.contains("## Goal\n\ndeploy service"));
        assert!(prompt.contains("1. write code\n"));
        assert!(prompt.contains("2. run tests\n"));
    }

    #[test]
    fn prompt_empty_tasks() {
        let prompt = build_validation_prompt("goal", &[]);
        assert!(prompt.contains("## Tasks to Validate\n\n"));
        assert!(!prompt.contains("1."));
    }

    // --- build_assessment_prompt ---

    #[test]
    fn assessment_prompt_includes_task_and_output() {
        let prompt = build_assessment_prompt("fix login", "done, all tests pass");
        assert!(prompt.contains("## Task\nfix login"));
        assert!(prompt.contains("## Output\ndone, all tests pass"));
        assert!(prompt.contains("ACCOMPLISHED"));
        assert!(prompt.contains("INCOMPLETE"));
    }

    // --- feedback_json ---

    #[test]
    fn feedback_json_basic() {
        let json = feedback_json("task incomplete");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed["hookSpecificOutput"]["hookEventName"],
            "PostToolUse"
        );
        let ctx = parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.contains("task incomplete"));
    }

    #[test]
    fn feedback_json_escapes_quotes() {
        let json = feedback_json(r#"missing "tests""#);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let ctx = parsed["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        assert!(ctx.contains(r#"missing "tests""#));
    }

    // --- contains_incomplete ---

    #[test]
    fn detects_incomplete() {
        assert!(contains_incomplete("INCOMPLETE: no tests added"));
    }

    #[test]
    fn detects_accomplished() {
        assert!(!contains_incomplete("ACCOMPLISHED"));
    }

    #[test]
    fn incomplete_must_be_line_start() {
        assert!(!contains_incomplete("task was INCOMPLETE maybe"));
    }

    #[test]
    fn incomplete_multiline() {
        assert!(contains_incomplete(
            "some preamble\nINCOMPLETE: partial\nmore"
        ));
    }

    // --- strip_frontmatter ---

    #[test]
    fn strip_yaml_frontmatter() {
        let input = "---\nname: test\n---\n\nBody content";
        assert_eq!(strip_frontmatter(input), "Body content");
    }

    #[test]
    fn no_frontmatter_unchanged() {
        let input = "Just body content";
        assert_eq!(strip_frontmatter(input), "Just body content");
    }

    #[test]
    fn unclosed_frontmatter_unchanged() {
        let input = "---\nname: test\nno closing";
        assert_eq!(strip_frontmatter(input), input);
    }

    #[test]
    fn frontmatter_trims_leading_whitespace() {
        let input = "---\nk: v\n---\n\n\n  Body";
        assert_eq!(strip_frontmatter(input), "Body");
    }
}
