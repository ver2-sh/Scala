//! Frozen v4 selection and ceilings. Unused budgets never change another task.
use super::{digest, suite};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkPlan {
    pub questions: Vec<String>,
    pub coding: Vec<String>,
    pub retrieval: Vec<String>,
    pub single: Vec<usize>,
    pub agents: Vec<usize>,
    pub probes: Vec<String>,
    pub preparation_seconds: u64,
    pub warmup_seconds: u64,
    pub probe_seconds: u64,
    pub question_seconds: u64,
    pub single_seconds: u64,
    pub agent_seconds: u64,
    pub stop_confirmation_seconds: u64,
    /// Maximum confirmations adding time outside the frozen task ceilings.
    pub maximum_stop_confirmations: u64,
    /// Frozen allowance for verification, checkpoints, persistence and progress.
    pub execution_bookkeeping_seconds: u64,
    pub cleanup_finalization_seconds: u64,
    pub hard_seconds: u64,
}
impl BenchmarkPlan {
    pub fn new() -> Result<Self, String> {
        let mut plan = Self {
            retrieval: super::retrieval::tasks()
                .into_iter()
                .map(|t| t.id)
                .collect(),
            coding: super::coding::tasks().into_iter().map(|t| t.id).collect(),
            questions: suite::questions().into_iter().map(|q| q.id).collect(),
            single: (0..8).collect(),
            agents: (0..4).collect(),
            probes: suite::probes().into_iter().map(|(id, _)| id).collect(),
            preparation_seconds: 55,
            warmup_seconds: 9,
            probe_seconds: 14,
            question_seconds: 7,
            single_seconds: 6,
            agent_seconds: 18,
            stop_confirmation_seconds: 1,
            maximum_stop_confirmations: 0,
            execution_bookkeeping_seconds: 15,
            cleanup_finalization_seconds: 16,
            hard_seconds: 600,
        };
        // Each task ends after its first confirmation path. Agent confirmation
        // inside the task ceiling can race that ceiling, but only the outer
        // timeout confirmation adds overhead beyond it. Warmup always executes.
        plan.maximum_stop_confirmations = 1 + plan.total_tasks() as u64;
        if plan.work_seconds()
            + plan.stop_confirmation_budget_seconds()
            + plan.execution_bookkeeping_seconds
            + plan.cleanup_finalization_seconds
            > plan.hard_seconds
        {
            return Err(
                "frozen phase ceilings exceed global deadline including stops, bookkeeping and cleanup/finalization"
                    .into(),
            );
        }
        Ok(plan)
    }
    pub fn stop_confirmation_budget_seconds(&self) -> u64 {
        self.stop_confirmation_seconds * self.maximum_stop_confirmations
    }
    pub fn work_seconds(&self) -> u64 {
        self.coding.len() as u64 * (super::coding::SECONDS + 1)
            + self.retrieval.len() as u64 * super::retrieval::SECONDS
            + self.preparation_seconds
            + self.warmup_seconds
            + self.probes.len() as u64 * self.probe_seconds
            + self.questions.len() as u64 * self.question_seconds
            + self.single.len() as u64 * self.single_seconds
            + self.agents.len() as u64 * self.agent_seconds
    }
    pub fn total_tasks(&self) -> usize {
        self.coding.len()
            + self.retrieval.len()
            + self.probes.len()
            + self.questions.len()
            + self.single.len()
            + self.agents.len()
    }
    pub fn task_ids(&self) -> Vec<String> {
        self.probes
            .iter()
            .cloned()
            .chain(self.questions.iter().cloned())
            .chain(self.coding.iter().cloned())
            .chain(self.single.iter().map(|i| format!("tool-{}", i + 1)))
            .chain(self.agents.iter().map(|i| format!("agent-{}", i + 1)))
            .chain(self.retrieval.iter().cloned())
            .collect()
    }
    pub fn manifest(&self) -> Value {
        let mut v = super::manifest();
        v["intelligence"] = json!(
            suite::questions()
                .into_iter()
                .filter(|q| self.questions.contains(&q.id))
                .map(|mut q| {
                    q.seconds = self.question_seconds;
                    q
                })
                .collect::<Vec<_>>()
        );
        v["single_tools"] = json!(
            suite::single_cases()
                .into_iter()
                .enumerate()
                .filter(|(i, _)| self.single.contains(i))
                .map(|(_, c)| c)
                .collect::<Vec<_>>()
        );
        v["agents"] = json!(
            self.agents
                .iter()
                .map(|i| json!({"id":format!("agent-{}",i+1),"prompt":suite::AGENT_PROMPTS[*i]}))
                .collect::<Vec<_>>()
        );
        v["fixture"] = json!(
            self.agents
                .iter()
                .map(|i| suite::Fixture::new(*i))
                .collect::<Vec<_>>()
        );
        v["probes"]=json!(suite::probes().into_iter().filter(|(id,_)|self.probes.contains(id)).map(|(id,input)|json!({"id":id,"utf8_bytes":input.len(),"unicode_characters":input.chars().count(),"input":input,"seconds":self.probe_seconds,"max_output_tokens":suite::PROBE_TOKENS})).collect::<Vec<_>>());
        v["warmup"]["seconds"] = json!(self.warmup_seconds);
        v["agent_limits"]["single_seconds"] = json!(self.single_seconds);
        v["agent_limits"]["multi_seconds"] = json!(self.agent_seconds);
        v["plan"] = json!(self);
        v["plan_hash"] = json!(digest(self));
        v["selected_task_ids"] = json!(self.task_ids());
        v["phase_work_seconds"] = json!(self.work_seconds());
        v["stop_confirmation_budget_seconds"] = json!(self.stop_confirmation_budget_seconds());
        v["execution_bookkeeping_seconds"] = json!(self.execution_bookkeeping_seconds);
        v["cleanup_finalization_seconds"] = json!(self.cleanup_finalization_seconds);
        v["hard_seconds"] = json!(self.hard_seconds);
        v["headroom_seconds"] = json!(self.hard_seconds - self.work_seconds());
        v
    }
}
