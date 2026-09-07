//! Frozen v4 selection and ceilings. Unused budgets never change another task.
use super::{digest, suite};
use norted_core::BenchmarkCapability;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkMode {
    Quick,
    #[default]
    Standard,
}
impl std::str::FromStr for BenchmarkMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "quick" => Ok(Self::Quick),
            "standard" => Ok(Self::Standard),
            _ => Err("mode must be quick or standard".into()),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkPlan {
    pub mode: BenchmarkMode,
    pub capabilities: BTreeSet<BenchmarkCapability>,
    pub questions: Vec<String>,
    pub single: Vec<usize>,
    pub agents: Vec<usize>,
    pub retrieval: Vec<usize>,
    pub context_targets: Vec<usize>,
    pub probes: Vec<String>,
    pub preparation_seconds: u64,
    pub warmup_seconds: u64,
    pub probe_seconds: u64,
    pub question_seconds: u64,
    pub single_seconds: u64,
    pub agent_seconds: u64,
    pub retrieval_seconds: u64,
    pub context_seconds: u64,
    pub hard_seconds: u64,
}
impl BenchmarkPlan {
    pub fn new(
        mode: BenchmarkMode,
        capabilities: BTreeSet<BenchmarkCapability>,
    ) -> Result<Self, String> {
        use BenchmarkCapability::*;
        if capabilities.contains(&Retrieval) && !capabilities.contains(&ToolUse) {
            return Err("retrieval requires tool_use".into());
        }
        let quick = mode == BenchmarkMode::Quick;
        let questions = suite::questions()
            .into_iter()
            .filter(|q| {
                capabilities.contains(&if q.category == "code" {
                    Coding
                } else {
                    Reasoning
                }) && (!quick || q.id.ends_with("-1") || q.id.ends_with("-4"))
            })
            .map(|q| q.id)
            .collect();
        let tool = capabilities.contains(&ToolUse);
        let retrieval = capabilities.contains(&Retrieval);
        let context = capabilities.contains(&LongContext);
        let plan = Self {
            mode,
            capabilities,
            questions,
            single: if tool {
                if quick {
                    vec![0, 5, 7]
                } else {
                    (0..8).collect()
                }
            } else {
                vec![]
            },
            agents: if tool {
                if quick { vec![2] } else { (0..4).collect() }
            } else {
                vec![]
            },
            retrieval: if retrieval {
                if quick {
                    vec![0, 3, 6]
                } else {
                    (0..8).collect()
                }
            } else {
                vec![]
            },
            context_targets: if context {
                if quick {
                    vec![4096, 16384]
                } else {
                    vec![4096, 16384, 32768, 65536, 131072]
                }
            } else {
                vec![]
            },
            probes: if quick {
                vec!["short-1".into(), "medium-1".into()]
            } else {
                suite::probes().into_iter().map(|(id, _)| id).collect()
            },
            preparation_seconds: if quick { 30 } else { 60 },
            warmup_seconds: if quick { 3 } else { 5 },
            probe_seconds: if quick { 12 } else { 20 },
            question_seconds: if quick { 3 } else { 6 },
            single_seconds: if quick { 3 } else { 4 },
            agent_seconds: if quick { 10 } else { 18 },
            retrieval_seconds: if quick { 10 } else { 12 },
            context_seconds: if quick { 5 } else { 14 },
            hard_seconds: if quick { 180 } else { 600 },
        };
        if plan.work_seconds() + 16 > plan.hard_seconds {
            return Err(
                "frozen phase ceilings exceed global deadline including cleanup/finalization"
                    .into(),
            );
        }
        Ok(plan)
    }
    pub fn work_seconds(&self) -> u64 {
        self.preparation_seconds
            + self.warmup_seconds
            + self.probes.len() as u64 * self.probe_seconds
            + self.questions.len() as u64 * self.question_seconds
            + self.single.len() as u64 * self.single_seconds
            + self.agents.len() as u64 * self.agent_seconds
            + self.retrieval.len() as u64 * self.retrieval_seconds
            + self.context_targets.len() as u64 * self.context_seconds
    }
    pub fn total_tasks(&self) -> usize {
        self.probes.len()
            + self.questions.len()
            + self.single.len()
            + self.agents.len()
            + self.retrieval.len()
            + self.context_targets.len()
    }
    pub fn task_ids(&self) -> Vec<String> {
        self.probes
            .iter()
            .cloned()
            .chain(self.questions.iter().cloned())
            .chain(self.single.iter().map(|i| format!("tool-{}", i + 1)))
            .chain(self.agents.iter().map(|i| format!("agent-{}", i + 1)))
            .chain(
                self.retrieval
                    .iter()
                    .map(|i| format!("retrieval-{}", i + 1)),
            )
            .chain(self.context_targets.iter().map(|n| format!("ladder-{n}")))
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
        v["headroom_seconds"] = json!(self.hard_seconds - self.work_seconds());
        v["retrieval"] = json!({"tasks":super::retrieval::tasks().into_iter().enumerate().filter(|(i,_)|self.retrieval.contains(i)).map(|(_,t)|t).collect::<Vec<_>>(),"repository":super::retrieval::repository(),"tools":super::retrieval::tools(),"rubric":"file-line-f0.5-grounded/1","rounds":6,"calls":10});
        v["context_ladder"] = json!({"rubric":"exact-json-context/1","payload_version":1,"size_unit":"target Unicode characters, not native tokens","admission":"payload UTF-8 bytes plus 2048 overhead must fit observed context limit; unknown limit unavailable","useful_context_rule":"highest successfully completed rung with score >= 0.8 times positive low-rung baseline; no inference for untested rungs","tasks":self.context_targets.iter().map(|n| super::context::payload(*n,self.capabilities.contains(&BenchmarkCapability::Retrieval))).collect::<Vec<_>>()});
        v
    }
}
