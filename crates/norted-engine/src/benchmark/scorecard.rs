use super::*;

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
fn binary(run: &Run, categories: &[&str], required: usize) -> Value {
    let items = run
        .evidence
        .iter()
        .filter(|e| categories.contains(&e.category.as_str()))
        .collect::<Vec<_>>();
    let n = items.iter().filter(|e| e.score.is_some()).count();
    let pass = items.iter().filter(|e| e.score == Some(1.0)).count();
    json!({"state":if n==required&&required>0{"complete"}else{"incomplete"},"score":(finished(&run.status)&&n==required&&required>0).then(||100.0*pass as f64/n as f64),"passed":pass,"attempted":items.iter().filter(|e|e.status!="unavailable").count(),"scored":n,"required":required,"n":n,"wilson_95":wilson(pass,n),"task_ids":items.iter().map(|e|&e.id).collect::<Vec<_>>()})
}
pub(super) fn build(run: &Run, _categories: &BTreeMap<String, Value>) -> Value {
    if run.methodology != suite::METHOD {
        return json!({"methodology":"historical methodology; original evidence only"});
    }
    let intelligence = binary(run, &["logic", "context", "code", "instruction"], 24);
    let single = binary(run, &["tool"], 8);
    let multi = binary(run, &["agent"], 4);
    let agentic = single["score"]
        .as_f64()
        .zip(multi["score"].as_f64())
        .map(|(a, b)| (a + b) / 2.0);
    json!({"retrieval":retrieval(run),"intelligence":intelligence,"coding":binary(run,&["coding"],6),"agentic":{"score":agentic,"single":single,"multi":multi},"methodology":suite::METHOD})
}

fn retrieval(run: &Run) -> Value {
    if !run.manifest["retrieval"].is_object() {
        return Value::Null;
    }
    let items = run
        .evidence
        .iter()
        .filter(|e| e.category == "retrieval")
        .collect::<Vec<_>>();
    let unavailable = items.iter().find(|e| e.status == "unavailable");
    let complete = unavailable.is_none()
        && items.len() == super::retrieval::tasks().len()
        && items.iter().all(|e| e.score.is_some());
    let mut raw = json!({});
    for key in [
        "file_precision",
        "file_recall",
        "file_f05",
        "line_precision",
        "line_recall",
        "line_f05",
    ] {
        let values = items
            .iter()
            .filter_map(|e| e.request_overrides["retrieval_metrics"][key].as_f64())
            .collect::<Vec<_>>();
        raw[key] =
            json!((!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64));
    }
    for key in [
        "polluting_lines",
        "returned_lines",
        "tool_calls",
        "serial_rounds",
        "truncated_results",
        "retrieval_wall_seconds",
    ] {
        raw[key] = json!(
            items
                .iter()
                .filter_map(|e| e.request_overrides["retrieval_metrics"][key].as_f64())
                .sum::<f64>()
        );
    }
    for key in [
        "success",
        "failure",
        "malformed_final",
        "tool_protocol_failure",
    ] {
        raw[key] = json!(
            items
                .iter()
                .filter(|e| e.request_overrides["retrieval_metrics"][key] == true)
                .count()
        );
    }
    let observed = items
        .iter()
        .filter(|e| e.request_overrides["retrieval_metrics"].is_object())
        .count();
    if observed == 0 {
        raw = Value::Null;
    } else {
        raw["observed_tasks"] = json!(observed);
    }
    if observed > 0 {
        raw["aggregation"] = json!(
            "task macro means for P/R/F0.5; sums for counts/time; partial evidence never supplies a headline"
        );
    }
    json!({"score":(finished(&run.status)&&complete).then(||100.0*items.iter().filter_map(|e|e.score).sum::<f64>()/super::retrieval::tasks().len() as f64),"state":if unavailable.is_some(){"unavailable"}else if complete{"complete"}else{"incomplete"},"reason":unavailable.map(|e|&e.explanation),"required":super::retrieval::tasks().len(),"scored":items.iter().filter(|e|e.score.is_some()).count(),"raw":raw,"tasks":items.iter().map(|e|json!({"id":e.id,"status":e.status,"score":e.score.map(|v|100.0*v),"reason":e.explanation,"raw":e.request_overrides["retrieval_metrics"]})).collect::<Vec<_>>()})
}
