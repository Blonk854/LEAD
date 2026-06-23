use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Persistent goal attached to an agent thread (Codex-style `/goal` mode).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadGoal {
    pub objective: String,
    pub success_criteria: String,
    pub status: GoalStatus,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub tokens_used: u64,
    #[serde(default)]
    pub wall_clock_budget: Option<u64>,
    pub started_at: DateTime<Utc>,
    #[serde(default)]
    pub continuation_count: u32,
    #[serde(default)]
    pub checkpoints: Vec<GoalCheckpoint>,
    /// Continuation count when the last checkpoint was written.
    #[serde(default)]
    pub last_checkpoint_continuation: u32,
}

const MAX_STORED_GOAL_CHECKPOINTS: usize = 10;
const MAX_CHECKPOINTS_IN_PROMPT: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Paused,
    BudgetLimited,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalCheckpoint {
    pub at: DateTime<Utc>,
    pub summary: String,
}

impl ThreadGoal {
    pub fn new(objective: String, success_criteria: String, token_budget: Option<u64>) -> Self {
        Self {
            objective,
            success_criteria,
            status: GoalStatus::Active,
            token_budget,
            tokens_used: 0,
            wall_clock_budget: None,
            started_at: Utc::now(),
            continuation_count: 0,
            checkpoints: Vec::new(),
            last_checkpoint_continuation: 0,
        }
    }

    pub fn push_checkpoint(&mut self, summary: String) {
        self.checkpoints.push(GoalCheckpoint {
            at: Utc::now(),
            summary,
        });
        if self.checkpoints.len() > MAX_STORED_GOAL_CHECKPOINTS {
            let excess = self.checkpoints.len() - MAX_STORED_GOAL_CHECKPOINTS;
            self.checkpoints.drain(0..excess);
        }
        self.last_checkpoint_continuation = self.continuation_count;
    }

    pub fn should_write_checkpoint(&self) -> bool {
        self.is_active()
            && self.continuation_count > 0
            && self.continuation_count >= self.last_checkpoint_continuation.saturating_add(2)
    }

    fn checkpoint_context(&self) -> String {
        if self.checkpoints.is_empty() {
            return String::new();
        }

        let entries: Vec<String> = self
            .checkpoints
            .iter()
            .rev()
            .take(MAX_CHECKPOINTS_IN_PROMPT)
            .map(|checkpoint| checkpoint.summary.clone())
            .collect();

        format!(
            "\n\nRecent progress checkpoints (newest first):\n{}",
            entries
                .into_iter()
                .enumerate()
                .map(|(index, summary)| format!("{}. {summary}", index + 1))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }

    pub fn is_active(&self) -> bool {
        self.status == GoalStatus::Active
    }

    pub fn within_token_budget(&self) -> bool {
        self.token_budget
            .is_none_or(|budget| self.tokens_used < budget)
    }

    pub fn record_tokens(&mut self, tokens: u64) {
        self.tokens_used = self.tokens_used.saturating_add(tokens);
        if !self.within_token_budget() && self.status == GoalStatus::Active {
            self.status = GoalStatus::BudgetLimited;
        }
    }

    pub fn status_summary(&self) -> String {
        let budget = self
            .token_budget
            .map(|b| format!("{}/{} tokens used", self.tokens_used, b))
            .unwrap_or_else(|| format!("{} tokens used", self.tokens_used));

        let checkpoints = if self.checkpoints.is_empty() {
            String::new()
        } else {
            format!("\nCheckpoints recorded: {}", self.checkpoints.len())
        };

        format!(
            "Goal status: {:?}\nObjective: {}\nSuccess criteria: {}\n{}\nContinuations: {}{}",
            self.status, self.objective, self.success_criteria, budget, self.continuation_count,
            checkpoints
        )
    }

    pub fn continuation_prompt(&self) -> String {
        format!(
            "You are working toward an active goal. Do not stop until the success criteria are \
             verified.\n\nObjective: {}\n\nSuccess criteria: {}{}\n\nReview your progress, run \
             any verification steps needed, and continue working. Only end your turn when the goal \
             is fully achieved or you are genuinely blocked and need user input.",
            self.objective,
            self.success_criteria,
            self.checkpoint_context()
        )
    }

    pub fn verification_prompt(&self) -> String {
        format!(
            "Before ending, verify the goal against its success criteria.\n\nObjective: {}\n\n\
             Success criteria: {}{}\n\nRun the checks needed to confirm completion. If not met, \
             continue working.",
            self.objective,
            self.success_criteria,
            self.checkpoint_context()
        )
    }
}

/// Parsed `/goal` slash command.
#[derive(Debug, PartialEq, Eq)]
pub enum GoalCommand {
    Set {
        objective: String,
        token_budget: Option<u64>,
    },
    Status,
    Pause,
    Resume,
    Clear,
}

impl GoalCommand {
    pub fn parse(input: &str) -> Self {
        let input = input.trim();
        if input.is_empty() {
            return GoalCommand::Status;
        }

        let mut parts = input.split_whitespace();
        let first = parts.next().unwrap_or("");

        match first {
            "pause" => GoalCommand::Pause,
            "resume" => GoalCommand::Resume,
            "clear" => GoalCommand::Clear,
            "status" => GoalCommand::Status,
            _ => {
                let mut token_budget = None;
                let mut rest: Vec<&str> = Vec::new();
                let mut iter = std::iter::once(first).chain(parts);
                while let Some(part) = iter.next() {
                    if part == "--budget" {
                        if let Some(budget) = iter.next().and_then(|s| s.parse().ok()) {
                            token_budget = Some(budget);
                        }
                    } else {
                        rest.push(part);
                    }
                }
                let objective = rest.join(" ");
                if objective.is_empty() {
                    GoalCommand::Status
                } else {
                    GoalCommand::Set {
                        objective,
                        token_budget,
                    }
                }
            }
        }
    }
}

#[allow(dead_code)]
pub fn default_wall_clock_budget() -> Duration {
    Duration::from_secs(4 * 60 * 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_goal_commands() {
        assert_eq!(GoalCommand::parse(""), GoalCommand::Status);
        assert_eq!(GoalCommand::parse("pause"), GoalCommand::Pause);
        assert_eq!(GoalCommand::parse("clear"), GoalCommand::Clear);
        assert_eq!(
            GoalCommand::parse("fix the failing tests"),
            GoalCommand::Set {
                objective: "fix the failing tests".into(),
                token_budget: None,
            }
        );
        assert_eq!(
            GoalCommand::parse("--budget 100000 ship feature X"),
            GoalCommand::Set {
                objective: "ship feature X".into(),
                token_budget: Some(100000),
            }
        );
    }

    #[test]
    fn test_token_budget_marks_limited() {
        let mut goal = ThreadGoal::new("ship it".into(), "done".into(), Some(100));
        goal.record_tokens(50);
        assert_eq!(goal.status, GoalStatus::Active);
        assert!(goal.within_token_budget());

        goal.record_tokens(60);
        assert_eq!(goal.status, GoalStatus::BudgetLimited);
        assert!(!goal.within_token_budget());
    }

    #[test]
    fn test_no_budget_never_limits() {
        let mut goal = ThreadGoal::new("ship it".into(), "done".into(), None);
        goal.record_tokens(1_000_000);
        assert_eq!(goal.status, GoalStatus::Active);
        assert!(goal.within_token_budget());
    }

    #[test]
    fn test_goal_prompts_include_objective() {
        let goal = ThreadGoal::new(
            "read Cargo.toml".into(),
            "file contents reported".into(),
            None,
        );
        assert!(goal.continuation_prompt().contains("read Cargo.toml"));
        assert!(goal.verification_prompt().contains("read Cargo.toml"));
        assert!(goal.status_summary().contains("Continuations: 0"));
    }

    #[test]
    fn test_checkpoint_context_in_prompts() {
        let mut goal = ThreadGoal::new("ship".into(), "tests pass".into(), None);
        goal.push_checkpoint("Fixed login bug".into());
        assert!(goal.continuation_prompt().contains("Fixed login bug"));
        assert!(goal.verification_prompt().contains("Fixed login bug"));
    }

    #[test]
    fn test_clone_preserves_state_for_rollover() {
        // A rollover carries the goal to a fresh thread by cloning it, so the
        // clone must preserve objective, success criteria, budget, tokens used,
        // and checkpoints so the new thread resumes seamlessly.
        let mut goal = ThreadGoal::new("ship feature".into(), "tests pass".into(), Some(100_000));
        goal.record_tokens(40_000);
        goal.continuation_count = 5;
        goal.push_checkpoint("implemented core logic".into());
        goal.push_checkpoint("wired up the UI".into());

        let carried = goal.clone();
        assert_eq!(carried.objective, "ship feature");
        assert_eq!(carried.success_criteria, "tests pass");
        assert_eq!(carried.token_budget, Some(100_000));
        assert_eq!(carried.tokens_used, 40_000);
        assert_eq!(carried.continuation_count, 5);
        assert_eq!(carried.checkpoints.len(), 2);
        assert!(carried.is_active());
        // Carried-over checkpoints feed the continuation prompt in the new thread.
        assert!(carried.continuation_prompt().contains("wired up the UI"));
    }

    #[test]
    fn test_should_write_checkpoint_every_two_continuations() {
        let mut goal = ThreadGoal::new("ship".into(), "done".into(), None);
        goal.continuation_count = 1;
        assert!(!goal.should_write_checkpoint());
        goal.continuation_count = 2;
        assert!(goal.should_write_checkpoint());
        goal.push_checkpoint("progress".into());
        goal.continuation_count = 3;
        assert!(!goal.should_write_checkpoint());
        goal.continuation_count = 4;
        assert!(goal.should_write_checkpoint());
    }
}
