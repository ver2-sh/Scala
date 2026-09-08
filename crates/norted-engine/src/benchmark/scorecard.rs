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
    json!({"intelligence":intelligence,"coding":binary(run,&["coding"],6),"agentic":{"score":agentic,"single":single,"multi":multi},"methodology":suite::METHOD})
}
