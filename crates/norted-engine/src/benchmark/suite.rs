//! Frozen, offline inputs and binary oracles for Norted Quick Bench v4.
use crate::{InferenceTool, InferenceToolCall};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const INTELLIGENCE: &str = include_str!("intelligence.json");
pub const SUITE: &str = "norted-quick-bench/4";
pub const METHOD: &str = "json-fixture-executable-native-prefill/4";
pub const PROBE_SECONDS: u64 = 16;
pub const PROBE_TOKENS: u32 = 2048;
pub const POLICY: &str = "Frozen fixed-denominator plan; one attempt per task; no retries or budget reallocation. Fixed full task pack <=600s including preparation. Coding: 7s inference plus 1s evaluation per task. Execution stops 16s before hard deadline; cleanup by hard deadline minus 1s; immutable finalization by hard deadline. Cancellation confirmation and bookkeeping share headroom. Unknown measurements remain unavailable. Internal managed streaming boundary; nonce prefix; cache unverified.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub category: String,
    pub prompt: String,
    pub answer: Value,
    pub rubric: String,
    pub seconds: u64,
    pub max_output_tokens: u32,
}

pub fn questions() -> Vec<Question> {
    serde_json::from_str(INTELLIGENCE).expect("bundled intelligence pack")
}

/// Answers use scalar/array JSON only. Complete parsing rejects commentary,
/// multiple answers and substring matches. No object duplicate-key ambiguity.
pub fn grade_json(response: &str, expected: &Value) -> bool {
    serde_json::from_str::<Value>(response.trim()).is_ok_and(|v| v == *expected)
}

pub fn tools() -> Vec<InferenceTool> {
    [
        ("read", "Read a virtual key and its revision.", json!({"key":{"type":"string"}}), vec!["key"]),
        ("read_many", "Read 1 to 3 distinct virtual keys in the given order.", json!({"keys":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":3,"uniqueItems":true}}), vec!["keys"]),
        ("search", "List virtual keys whose names contain the query, sorted.", json!({"query":{"type":"string"}}), vec!["query"]),
        ("update", "Replace a virtual string value using its current integer revision. Conflicts require reading again.", json!({"key":{"type":"string"},"revision":{"type":"integer","minimum":0},"value":{"type":"string"}}), vec!["key","revision","value"]),
        ("verify", "Read back a virtual key and record that its current state was verified.", json!({"key":{"type":"string"}}), vec!["key"]),
    ].into_iter().map(|(name,description,properties,required)| InferenceTool {
        name:name.into(), description:Some(description.into()),
        parameters:json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
    }).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCase {
    pub id: String,
    pub prompt: String,
    pub name: Option<String>,
    pub arguments: Value,
    pub answer: Option<Value>,
}

pub fn single_cases() -> Vec<ToolCase> {
    let rows = [
        (
            "Read the virtual key config/port. Do not modify anything.",
            Some("read"),
            json!({"key":"config/port"}),
            None,
        ),
        (
            "Find keys whose names contain config/. Do not read or change their values.",
            Some("search"),
            json!({"query":"config/"}),
            None,
        ),
        (
            "Read config/port and config/mode together in that order using one call.",
            Some("read_many"),
            json!({"keys":["config/port","config/mode"]}),
            None,
        ),
        (
            "Update config/mode to safe using its supplied current revision 0. Do not read first in this single-call task.",
            Some("update"),
            json!({"key":"config/mode","revision":0,"value":"safe"}),
            None,
        ),
        (
            "Verify the current state of config/port without changing it.",
            Some("verify"),
            json!({"key":"config/port"}),
            None,
        ),
        (
            "No fixture information is needed: return the JSON integer for 7 minus 3. Do not use a tool.",
            None,
            Value::Null,
            Some(json!(4)),
        ),
        (
            "Read exactly these three keys in one call, in order: config/mode, config/port, target. Do not include other keys.",
            Some("read_many"),
            json!({"keys":["config/mode","config/port","target"]}),
            None,
        ),
        (
            "Set config/mode to the empty string, using the supplied revision 0. Make exactly one update call.",
            Some("update"),
            json!({"key":"config/mode","revision":0,"value":""}),
            None,
        ),
    ];
    rows.into_iter()
        .enumerate()
        .map(|(i, (prompt, name, arguments, answer))| ToolCase {
            id: format!("tool-{}", i + 1),
            prompt: prompt.into(),
            name: name.map(str::to_owned),
            arguments,
            answer,
        })
        .collect()
}

pub fn grade_single(case: &ToolCase, text: &str, calls: &[InferenceToolCall]) -> bool {
    match &case.name {
        None => calls.is_empty() && case.answer.as_ref().is_some_and(|a| grade_json(text, a)),
        Some(name) => {
            calls.len() == 1
                && !calls[0].id.is_empty()
                && calls[0].name == *name
                && parse_arguments(&calls[0].arguments).is_ok_and(|v| v == case.arguments)
        }
    }
}

// A custom map visitor rejects duplicate model-generated keys instead of
// accepting the last value. Nested argument values in this suite are scalars/lists.
pub fn parse_arguments(text: &str) -> Result<Value, String> {
    struct Unique;
    impl<'de> serde::de::Visitor<'de> for Unique {
        type Value = Value;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("unique argument keys")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
            let mut result = serde_json::Map::new();
            while let Some((k, v)) = map.next_entry::<String, Value>()? {
                if result.insert(k, v).is_some() {
                    return Err(serde::de::Error::custom("duplicate argument"));
                }
            }
            Ok(Value::Object(result))
        }
    }
    use serde::de::Deserializer;
    let mut d = serde_json::Deserializer::from_str(text);
    let value = (&mut d)
        .deserialize_map(Unique)
        .map_err(|e| e.to_string())?;
    d.end().map_err(|e| e.to_string())?;
    Ok(value)
}

pub const AGENT_PROMPTS: [&str; 4] = [
    "Fixture calls in one response execute in listed order, but their results are available only in your next response. Use returned information before dependent actions; verify in a response after the update. Inspect config/mode before acting. Change it to safe using the observed revision, then verify the changed state.",
    "Fixture calls in one response execute in listed order, but their results are available only in your next response. Use returned information before dependent actions; verify in a response after the update. Read target to discover which key to change and desired to discover the new value. Inspect the target key, update it using its revision, then verify the changed state. You may combine independent reads.",
    "Fixture calls in one response execute in listed order, but their results are available only in your next response. Use returned information before dependent actions; verify in a response after the update. Inspect config/mode, change it to safe, and verify. A concurrent fixture writer will cause the first update to conflict. Recover by reading the current revision and then retrying the update.",
    "Fixture calls in one response execute in listed order, but their results are available only in your next response. Use returned information before dependent actions; verify in a response after the update. Search for keys containing service/. Inspect the returned service key, change its value to enabled using its revision, and verify the changed state.",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fixture {
    case: usize,
    values: BTreeMap<String, (String, u64)>,
    inspected: BTreeSet<String>,
    observed_revisions: BTreeMap<String, u64>,
    delivered_revisions: BTreeMap<String, u64>,
    delivered_search: bool,
    delivered_changed: BTreeSet<String>,
    verified: BTreeSet<String>,
    changed: BTreeSet<String>,
    searched: bool,
    conflict: bool,
    conflict_seen: bool,
}
impl Fixture {
    pub fn new(case: usize) -> Self {
        Self {
            case,
            values: [
                ("config/mode", ("fast", 0)),
                ("config/port", ("8080", 0)),
                ("target", ("config/port", 0)),
                ("desired", ("9090", 0)),
                ("service/worker", ("disabled", 0)),
            ]
            .into_iter()
            .map(|(k, (v, r))| (k.into(), (v.into(), r)))
            .collect(),
            inspected: BTreeSet::new(),
            observed_revisions: BTreeMap::new(),
            delivered_revisions: BTreeMap::new(),
            delivered_search: false,
            delivered_changed: BTreeSet::new(),
            verified: BTreeSet::new(),
            changed: BTreeSet::new(),
            searched: false,
            conflict: case == 2,
            conflict_seen: false,
        }
    }
    /// Freeze only information returned before this model response. Mutations
    /// during a batch cannot retroactively give its arguments observation credit.
    pub fn begin_response(&mut self) {
        self.delivered_revisions = self.observed_revisions.clone();
        self.delivered_search = self.searched;
        self.delivered_changed = self.changed.clone();
    }
    pub fn apply(&mut self, call: &InferenceToolCall) -> Result<Value, String> {
        let args = parse_arguments(&call.arguments)?;
        let object = args.as_object().ok_or("arguments must be an object")?;
        let required: &[&str] = match call.name.as_str() {
            "read" | "verify" => &["key"],
            "read_many" => &["keys"],
            "search" => &["query"],
            "update" => &["key", "revision", "value"],
            _ => return Err("unknown fixture tool".into()),
        };
        if object.len() != required.len() || required.iter().any(|k| !object.contains_key(*k)) {
            return Err("wrong argument keys".into());
        }
        if call.name == "search" {
            let q = args["query"].as_str().ok_or("query must be a string")?;
            self.searched |= "service/worker".contains(q);
            return Ok(json!(
                self.values
                    .keys()
                    .filter(|k| k.contains(q))
                    .collect::<Vec<_>>()
            ));
        }
        if call.name == "read_many" {
            let keys = args["keys"].as_array().ok_or("keys must be an array")?;
            let names = keys
                .iter()
                .map(|v| v.as_str().ok_or("key must be a string"))
                .collect::<Result<Vec<_>, _>>()?;
            if names.is_empty()
                || names.len() > 3
                || names.iter().collect::<BTreeSet<_>>().len() != names.len()
            {
                return Err("need 1 to 3 distinct keys".into());
            }
            if names.iter().any(|k| !self.values.contains_key(*k)) {
                return Err("unknown virtual key".into());
            }
            return Ok(Value::Array(
                names
                    .into_iter()
                    .map(|k| {
                        self.inspected.insert(k.into());
                        let (v, r) = &self.values[k];
                        if (self.case != 1
                            || k != "config/port"
                            || self.delivered_revisions.contains_key("target"))
                            && (self.case != 3 || k != "service/worker" || self.delivered_search)
                        {
                            self.observed_revisions.insert(k.into(), *r);
                        }
                        json!({"key":k,"value":v,"revision":r})
                    })
                    .collect(),
            ));
        }
        let key = args["key"].as_str().ok_or("key must be a string")?;
        let (value, revision) = self.values.get_mut(key).ok_or("unknown virtual key")?;
        match call.name.as_str() {
            "read" => {
                self.inspected.insert(key.into());
                if (self.case != 1
                    || key != "config/port"
                    || self.delivered_revisions.contains_key("target"))
                    && (self.case != 3 || key != "service/worker" || self.delivered_search)
                {
                    self.observed_revisions.insert(key.into(), *revision);
                }
            }
            "verify" => {
                self.inspected.insert(key.into());
                if self.delivered_changed.contains(key) {
                    self.verified.insert(key.into());
                }
            }
            "update" => {
                let r = args["revision"]
                    .as_u64()
                    .ok_or("revision must be a nonnegative integer")?;
                let v = args["value"].as_str().ok_or("value must be a string")?;
                if v.len() > 1024 {
                    return Err("value too large".into());
                }
                if self.conflict {
                    self.conflict = false;
                    self.conflict_seen = true;
                    *revision += 1;
                    self.inspected.remove(key);
                    self.observed_revisions.remove(key);
                    self.changed.remove(key);
                    return Err("revision conflict: read again".into());
                }
                if r != *revision {
                    return Err("revision conflict: read again".into());
                }
                if self.delivered_revisions.get(key) == Some(revision)
                    && (self.case != 1
                        || (self.delivered_revisions.contains_key("target")
                            && self.delivered_revisions.contains_key("desired")))
                    && (self.case != 3 || self.delivered_search)
                {
                    self.changed.insert(key.into());
                } else {
                    self.changed.remove(key);
                }
                self.observed_revisions.remove(key);
                self.verified.remove(key);
                *value = v.into();
                *revision += 1;
            }
            _ => unreachable!(),
        }
        Ok(json!({"key":key,"value":value,"revision":revision}))
    }
    pub fn solved(&self, case: usize) -> bool {
        let (key, wanted) = match case {
            1 => ("config/port", "9090"),
            3 => ("service/worker", "enabled"),
            _ => ("config/mode", "safe"),
        };
        self.values[key].0 == wanted
            && self.changed.contains(key)
            && self.verified.contains(key)
            && (case != 1
                || (self.inspected.contains("target") && self.inspected.contains("desired")))
            && (case != 2 || self.conflict_seen)
            && (case != 3 || self.searched)
            && self.values.iter().all(|(k, (_, r))| k == key || *r == 0)
    }
}

pub fn probes() -> Vec<(String, String)> {
    // Performance measures delivery, not another reasoning rubric. A fixed
    // copy workload provides the same output population after both contexts,
    // while preserving each profile's reasoning and generation settings.
    let passage1 = "A bounded work queue places a fixed limit on waiting jobs. When arrivals exceed available capacity, the service must decide whether to reject new work, delay admission, or ask the caller to try again later. This decision should be explicit so that overload does not silently become an ever growing memory allocation. A worker takes one admitted job, performs its operation, records the result, and releases the capacity associated with that job. Cancellation should remove work that has not started and notify running work when its result is no longer wanted. A deadline follows the job through each stage instead of restarting whenever the job enters another queue. Operators need separate measurements for arrivals, admissions, rejections, waiting time, active work, and completion time. Queue length alone cannot explain whether a service is healthy. A short queue may indicate low demand, rapid processing, or aggressive rejection. A long queue may hide a slow dependency even when local processors are mostly idle. Capacity changes should therefore be evaluated against the complete request path. Clear ownership of each job and a bounded shutdown procedure help the service stop without losing track of work that is still running.";
    let passage2 = "A deployment review compares observations from several services while keeping their measurement boundaries explicit. Request totals describe traffic volume, and error totals describe recorded failures within the same observation period. A combined error rate uses the sum of errors divided by the sum of requests. Individual service rates should not be averaged without accounting for their different traffic volumes. Reported latency medians describe the middle of each service distribution. They do not reveal the slowest requests, and their average is not the median of all requests combined. A release plan should record which version produced each observation, when collection started, and whether the traffic mix changed during the review. Independent checks can run together, but a check that relies on a migration must wait until that migration completes. A rollback plan must explain which data remains readable by the previous version and which operations require special handling. After a release, operators should compare rejection counts, queue age, error rates, and resource use with the earlier observation period. These measurements support investigation rather than proving a cause on their own. Keeping the original records makes later comparisons possible when new information changes the interpretation of an apparent improvement or regression.";
    let copy = |passage: &str| {
        format!(
            "Copy the passage below verbatim as your entire response, without a heading or quotation marks. The preceding context, if any, is background only.\n<passage>\n{passage}\n</passage>"
        )
    };
    let short1 = copy(passage1);
    let short2 = copy(passage2);
    let mut medium1 = "Background service records:\n".to_owned();
    let mut medium2 = "Background repository change records:\n".to_owned();
    for i in 0..48 {
        medium1.push_str(&format!("Service s{i:02}: workers={}, queue_limit={}, requests={}, failures={}, timeout_ms={}. Retries require an idempotency key; health checks exclude upstream latency.\n",2+i%7,32+8*(i%5),500+13*i,i%9,100+25*(i%6)));
        medium2.push_str(&format!("Change c{i:02}: module m{} now validates field f{} before storage; migration v{} supplies missing values. Release flag r{} controls activation. Rollback retains the prior reader; monitor rejected records and queue age.\n",i%9,i%13,1+i/8,i%5));
    }
    medium1.push_str(&copy(passage1));
    medium2.push_str(&copy(passage2));
    vec![
        ("short-1".into(), short1),
        ("short-2".into(), short2),
        ("medium-1".into(), medium1),
        ("medium-2".into(), medium2),
    ]
}
