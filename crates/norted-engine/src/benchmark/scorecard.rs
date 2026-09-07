use super::*;
use norted_core::BenchmarkCapability;

pub(super) fn wilson(successes: usize, n: usize) -> Value {
    if n == 0 {
        return Value::Null;
    }
    let n = n as f64;
    let p = successes as f64 / n;
    let z = 1.959963984540054;
    let center = (p + z * z / (2.0 * n)) / (1.0 + z * z / n);
    let half = z * (p * (1.0 - p) / n + z * z / (4.0 * n * n)).sqrt() / (1.0 + z * z / n);
    json!({"low":100.0*(center-half),"high":100.0*(center+half)})
}
fn stats(values: &[f64]) -> Value {
    let n = values.len();
    if n == 0 {
        return json!({"n":0});
    }
    let mean = values.iter().sum::<f64>() / n as f64;
    let sd = (n > 1)
        .then(|| (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt());
    json!({"n":n,"mean":mean,"standard_deviation":sd,"standard_error":sd.map(|v|v/(n as f64).sqrt()),"min":values.iter().copied().reduce(f64::min),"max":values.iter().copied().reduce(f64::max)})
}
fn binary(run: &Run, categories: &[&str], required: usize) -> Value {
    let items = run
        .evidence
        .iter()
        .filter(|e| categories.contains(&e.category.as_str()))
        .collect::<Vec<_>>();
    let n = items.iter().filter(|e| e.score.is_some()).count();
    let pass = items.iter().filter(|e| e.score == Some(1.0)).count();
    json!({"state":if n==required&&required>0{"complete"}else{"incomplete"},"score":(n==required&&required>0).then(||100.0*pass as f64/n as f64),"passed":pass,"attempted":items.iter().filter(|e|e.status!="unavailable").count(),"scored":n,"required":required,"n":n,"wilson_95":wilson(pass,n),"task_ids":items.iter().map(|e|&e.id).collect::<Vec<_>>()})
}
pub(super) fn build(run: &Run, _categories: &BTreeMap<String, Value>) -> Value {
    let Some(plan) = run.plan() else {
        return json!({"methodology":"legacy methodology; original scores only"});
    };
    let mut card = json!({"capabilities":plan.capabilities,"mode":plan.mode,"profile_quality":null,"confidence":if plan.mode==BenchmarkMode::Quick{"Quick: small confidence-check sample"}else{"Standard: small local task sample"}});
    let mut scores = Vec::new();
    for capability in &plan.capabilities {
        use BenchmarkCapability::*;
        let (name, mut category) = match capability {
            Reasoning => (
                "intelligence",
                binary(
                    run,
                    &["logic", "context", "instruction"],
                    plan.questions
                        .iter()
                        .filter(|q| !q.starts_with("code-"))
                        .count(),
                ),
            ),
            Coding => (
                "coding",
                binary(
                    run,
                    &["code"],
                    plan.questions
                        .iter()
                        .filter(|q| q.starts_with("code-"))
                        .count(),
                ),
            ),
            ToolUse => {
                let single = binary(run, &["tool"], plan.single.len());
                let multi = binary(run, &["agent"], plan.agents.len());
                let score = single["score"]
                    .as_f64()
                    .zip(multi["score"].as_f64())
                    .map(|(a, b)| (a + b) / 2.0);
                (
                    "agentic",
                    json!({"score":score,"state":if score.is_some(){"complete"}else{"incomplete"},"single":single,"multi":multi,"unavailable_reason":run.missing.iter().find(|s|s.starts_with("Agentic unavailable:"))}),
                )
            }
            Retrieval => {
                let items = run
                    .evidence
                    .iter()
                    .filter(|e| e.category == "retrieval")
                    .collect::<Vec<_>>();
                let values = items
                    .iter()
                    .filter_map(|e| e.score.map(|s| s * 100.0))
                    .collect::<Vec<_>>();
                let mut result = json!({"score":(values.len()==plan.retrieval.len()).then(||values.iter().sum::<f64>()/values.len().max(1) as f64),"state":if values.len()==plan.retrieval.len(){"complete"}else{"incomplete"},"attempted":items.iter().filter(|e|e.status!="unavailable").count(),"scored":values.len(),"required":plan.retrieval.len(),"sample":stats(&values),"tasks":items.iter().map(|e|json!({"id":e.id,"status":e.status,"metrics":e.request_overrides["retrieval_metrics"]})).collect::<Vec<_>>()});
                result["unavailable_reason"] = json!(
                    run.missing
                        .iter()
                        .find(|s| s.starts_with("Retrieval unavailable:"))
                );
                if !result["unavailable_reason"].is_null() {
                    result["state"] = json!("unavailable");
                }
                for field in [
                    "file_precision",
                    "file_recall",
                    "file_f0_5",
                    "line_precision",
                    "line_recall",
                    "line_f0_5",
                    "returned_lines",
                    "polluting_lines",
                ] {
                    result[field] = stats(
                        &items
                            .iter()
                            .filter_map(|e| {
                                e.request_overrides["retrieval_metrics"][field].as_f64()
                            })
                            .collect::<Vec<_>>(),
                    );
                }
                result["grounded_success_rate"] = if values.is_empty() {
                    Value::Null
                } else {
                    json!(
                        items
                            .iter()
                            .filter(
                                |e| e.request_overrides["retrieval_metrics"]["grounded_success"]
                                    == true
                            )
                            .count() as f64
                            / values.len() as f64
                    )
                };
                ("retrieval", result)
            }
            LongContext => {
                let items = run
                    .evidence
                    .iter()
                    .filter(|e| e.category == "ladder")
                    .collect::<Vec<_>>();
                let eligible = items
                    .iter()
                    .filter(|e| e.status != "unavailable")
                    .collect::<Vec<_>>();
                let values = eligible.iter().filter_map(|e| e.score).collect::<Vec<_>>();
                let baseline = items.first().and_then(|e| e.score).filter(|s| *s > 0.0);
                let useful = baseline.and_then(|base| {
                    items
                        .iter()
                        .filter(|e| {
                            e.status == "passed" && e.score.is_some_and(|s| s >= 0.8 * base)
                        })
                        .filter_map(|e| e.request_overrides["target_workload_characters"].as_u64())
                        .max()
                });
                let complete = !values.is_empty()
                    && values.len() == plan.context_targets.len()
                    && items.len() == plan.context_targets.len();
                (
                    "context",
                    json!({"score":complete.then(||100.0*values.iter().sum::<f64>()/values.len() as f64),"state":if complete{"complete"}else{"unavailable"},"required":plan.context_targets.len(),"attempted":eligible.len(),"observed_quality":stats(&values.iter().map(|s|s*100.0).collect::<Vec<_>>()),"scored":values.len(),"n":values.len(),"passed":values.iter().filter(|s|**s==1.0).count(),"wilson_95":wilson(values.iter().filter(|s|**s==1.0).count(),values.len()),"useful_context":useful,"unit":"target workload characters, not tokens","rungs":items.iter().map(|e|json!({"id":e.id,"score":e.score.map(|v|v*100.0),"status":e.status,"latency_ms":e.timing.completion_ms,"native_input_tokens":e.usage.as_ref().map(|u|u.input_tokens),"utf8_bytes":e.input_utf8_bytes,"unicode_characters":e.input_unicode_characters,"reason":e.explanation,"target_workload_characters":e.request_overrides["target_workload_characters"]})).collect::<Vec<_>>()}),
                )
            }
        };
        if let Some(score) = category["score"].as_f64() {
            scores.push(score);
        }
        if category["score"].is_null() {
            category["reason"] =
                json!("Declared capability lacks complete evidence; no redistribution");
        }
        card[name] = category;
    }
    card["profile_quality"] = if matches!(
        run.status.as_str(),
        "running" | "completed" | "completed_unavailable"
    ) && !scores.is_empty()
        && scores.len() == plan.capabilities.len()
    {
        json!(scores.iter().sum::<f64>() / scores.len() as f64)
    } else {
        Value::Null
    };
    let mut failures = BTreeMap::<String, usize>::new();
    let mut valid = 0;
    let mut attempted = 0;
    for e in &run.evidence {
        if matches!(e.category.as_str(), "speed" | "warmup")
            || matches!(
                e.status.as_str(),
                "unavailable" | "infrastructure_error" | "running" | "interrupted" | "unattempted"
            )
        {
            continue;
        }
        attempted += 1;
        let reason = if e.status == "timeout" {
            Some("timeout")
        } else {
            e.request_overrides["model_failure"].as_str()
        };
        if let Some(reason) = reason {
            *failures.entry(reason.into()).or_default() += 1;
        } else if e.request_overrides["valid_completion"] == true {
            valid += 1;
        } else {
            *failures.entry("invalid_completion".into()).or_default() += 1;
        }
    }
    card["reliability"] = json!({"score":(attempted>0).then(||100.0*valid as f64/attempted as f64),"valid":valid,"attempted":attempted,"model_failures":failures});
    card["cold_startup_load"] = json!({"seconds":if run.loaded_before{None}else{run.load_seconds},"reason":if run.loaded_before{Some("profile already resident")}else if run.load_seconds.is_none(){Some("load observation unavailable")}else{None}});
    card["efficiency"] = json!({"accelerator_binding":run.provenance.as_ref().and_then(|p|p.accelerator_binding.as_ref()),"host":run.environment["host"],"cpu_model":run.environment["cpu_model"],"logical_cpus":run.environment["logical_cpus"],"ram_total":run.environment["ram_total"],"peak_vram":null,"process_ram":null,"tps_per_gib":null,"reason":"No reliable process-scoped peak memory telemetry is supplied by the managed runtime contract; host total memory is not model usage"});
    card
}
