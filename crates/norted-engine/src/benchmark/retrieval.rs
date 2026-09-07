//! Original virtual source repository. Canonical ranges never enter model messages.
use crate::{InferenceTool, InferenceToolCall};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub fn repository() -> BTreeMap<String, String> {
    [
        ("src/api.rs", "use crate::cache::lookup;\nuse crate::auth::authorize;\npub fn get(req: Request, cfg: Config) -> Reply {\n    authorize(&req, &cfg)?;\n    let key = req.path.to_lowercase();\n    lookup(&key, cfg.cache_ttl)\n}\n"),
        ("src/cache.rs", "use crate::clock::now;\n// lookup preserves expired entries for diagnostics.\npub fn lookup(key: &str, ttl: u64) -> Reply {\n    let item = STORE.get(key)?;\n    if now() - item.created >= ttl { return Reply::Miss; }\n    Reply::Hit(item.body.clone())\n}\npub fn clear() { STORE.clear(); }\n"),
        ("src/config.rs", "pub struct Config { pub cache_ttl: u64, pub allow_guest: bool }\npub fn load(env: Env) -> Config {\n    Config {\n        cache_ttl: env.number(\"CACHE_TTL\").unwrap_or(60),\n        allow_guest: env.text(\"ALLOW_GUEST\") == Some(\"yes\"),\n    }\n}\n"),
        ("src/auth.rs", "pub fn authorize(req: &Request, cfg: &Config) -> Result {\n    if req.token.is_valid() { return Ok(()); }\n    if cfg.allow_guest && req.method == \"GET\" { return Ok(()); }\n    Err(Denied)\n}\n"),
        ("src/jobs/retry.rs", "pub fn next_delay(attempt: u32) -> u64 {\n    (5 * 2_u64.pow(attempt.min(6))).min(120)\n}\npub fn schedule(job: Job) {\n    if job.cancelled { return; }\n    QUEUE.push(job.id, next_delay(job.attempt));\n}\n"),
        ("src/ui/retry.rs", "// Display estimate only; does not schedule jobs.\npub fn next_delay(attempt: u32) -> String {\n    format!(\"try {}\", attempt + 1)\n}\n"),
        ("src/clock.rs", "pub fn now() -> u64 {\n    MONOTONIC.elapsed().as_secs()\n}\n"),
        ("src/export.rs", "pub fn export(rows: Vec<Row>) -> Vec<String> {\n    rows.into_iter()\n        .filter(|r| !r.deleted)\n        .filter(|r| r.visibility == Visibility::Public)\n        .map(|r| r.body)\n        .collect()\n}\n"),
        ("tests/cache.rs", "// Decoy: test helper is not production cache lookup.\nfn lookup(key: &str) -> Reply { Reply::Hit(key.into()) }\nfn expired() { assert_eq!(lookup(\"sample\"), Reply::Hit(\"sample\")); }\n"),
        ("docs/config.md", "# Historical configuration\nCACHE_TTL once defaulted to 30.\nALLOW_GUEST is described by the current source, not this historical note.\n"),
    ].into_iter().map(|(p,s)|(p.into(),s.into())).collect()
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub prompt: String,
    pub targets: Vec<Range>,
}
pub fn tasks() -> Vec<Task> {
    [
        ("Locate the production cache lookup implementation, including expiration and returned value.", vec![("src/cache.rs",3,7)]),
        ("Find the API code that normalizes the request key and delegates cache access, and the implementation it invokes.", vec![("src/api.rs",5,6),("src/cache.rs",3,7)]),
        ("Find the current source configuration deciding the cache lifetime and its default; ignore historical documentation.", vec![("src/config.rs",4,4)]),
        ("Find the worker retry delay calculation and the caller that skips cancelled jobs. Exclude display-only helpers.", vec![("src/jobs/retry.rs",1,7)]),
        ("Locate both the environment-controlled guest switch and the exact authorization rule using it.", vec![("src/config.rs",5,5),("src/auth.rs",1,5)]),
        ("Find which clock production cache expiry uses and where that clock is implemented.", vec![("src/cache.rs",5,5),("src/clock.rs",1,3)]),
        ("Find all source predicates excluding records from export; the relevant filters follow the export declaration.", vec![("src/export.rs",3,4)]),
        ("Find the request authorization call and the callee's token/guest/denial decision.", vec![("src/api.rs",4,4),("src/auth.rs",1,5)]),
    ].into_iter().enumerate().map(|(i,(p,r))|Task {id:format!("retrieval-{}",i+1),prompt:format!("{p}\nUse up to four retrieval rounds with up to eight independent calls per round. Evidence is capped at 320 source lines total. Return final ranges immediately when ready. Only after four tool rounds, a separate turn with no tools will request final ranges. Use the virtual repository tools to inspect evidence. Return only JSON {{\"ranges\":[{{\"path\":\"path\",\"start_line\":1,\"end_line\":2}}]}} with minimal inclusive 1-based source ranges. Do not include explanations."),targets:r.into_iter().map(|(f,start_line,end_line)|Range{path:f.into(),start_line,end_line}).collect()}).collect()
}
pub fn tools() -> Vec<InferenceTool> {
    [
        ("grep","Case-sensitive Rust regex search (no PCRE2), pattern 1..256 bytes. Optional path and include/exclude glob filters; sorted path/line/text results, at most 80 hits.",json!({"pattern":{"type":"string"},"path":{"type":"string"},"include":{"type":"string"},"exclude":{"type":"string"}}),vec!["pattern"]),
        ("glob","List sorted virtual paths. Pattern accepts * (any characters including /); no other wildcard syntax.",json!({"pattern":{"type":"string"}}),vec!["pattern"]),
        ("read","Read inclusive 1-based lines from a virtual path, maximum 80 lines. Oversized end/span returns available bounded prefix with truncated=true; invalid path/start/order is an error.",json!({"path":{"type":"string"},"start_line":{"type":"integer","minimum":1},"end_line":{"type":"integer","minimum":1}}),vec!["path","start_line","end_line"]),
    ].into_iter().map(|(name,desc,properties,required)|InferenceTool{name:name.into(),description:Some(desc.into()),parameters:json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})}).collect()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Search {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    include: Option<String>,
    #[serde(default)]
    exclude: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Glob {
    pattern: String,
}
pub const FINALIZATION: &str = "early-final-or-no-tools-json-schema-v1";

pub fn final_schema() -> Value {
    json!({
        "type": "object", "required": ["ranges"], "additionalProperties": false,
        "properties": {"ranges": {"type": "array", "maxItems": 16, "items": {
            "type": "object", "required": ["path", "start_line", "end_line"],
            "additionalProperties": false,
            "properties": {
                "path": {"type": "string", "minLength": 1, "maxLength": 256},
                "start_line": {"type": "integer", "minimum": 1, "maximum": 512},
                "end_line": {"type": "integer", "minimum": 1, "maximum": 512}
            }
        }}}
    })
}

pub fn finalize_request(request: &mut crate::InferenceRequest) {
    request.tools.clear();
    request.tool_choice = Some(crate::InferenceToolChoice::None);
    request.parallel_tool_calls = Some(false);
    request.output_format = Some(crate::OutputFormat::JsonSchema {
        name: Some("retrieval_ranges".into()),
        description: None,
        schema: final_schema(),
        strict: Some(true),
    });
    request.messages.push(crate::InferenceMessage::text(crate::InferenceRole::User,
        "Retrieval is complete: all four repository tool rounds have been used. No functions are available on this turn. Earlier tool calls describe past actions only. Return only canonical JSON {\"ranges\":[{\"path\":\"...\",\"start_line\":1,\"end_line\":2}]}. Select minimal ranges from gathered evidence, or an empty ranges array if none are relevant. No prose or tool calls."));
}

pub const RESULT_SERIALIZATION: &str = "path-lines-json-v1";

/// Canonical evidence stays independent of the model-facing wire layout.
pub struct ToolResult {
    files: BTreeMap<String, Vec<(usize, String)>>,
    truncated: bool,
    bounded: bool,
}

/// Only this boundary renders evidence for model messages and their audit log.
pub fn model_view(result: Result<ToolResult, String>) -> Value {
    match result {
        Ok(result) => {
            let mut value = json!({"files": result.files, "truncated": result.truncated});
            if result.bounded {
                value["bounded"] = json!(true);
            }
            value
        }
        Err(error) => json!({"error": error, "truncated": false}),
    }
}

/// Exact compact, sorted JSON wire text; typed evidence remains unescaped.
pub fn tool_text(value: &Value) -> String {
    let mut sorted = value.clone();
    sorted.sort_all_objects();
    sorted
        .to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
}

#[derive(Default)]
pub struct Fixture {
    evidence_lines: usize,
    pub located: BTreeSet<(String, usize)>,
}
impl Fixture {
    pub fn apply(&mut self, call: &InferenceToolCall) -> Result<ToolResult, String> {
        let repo = repository();
        let parse_error = |_: serde_json::Error| "invalid tool arguments".to_string();
        match call.name.as_str() {
            "grep" => {
                let a: Search = serde_json::from_str(&call.arguments).map_err(parse_error)?;
                if a.pattern.is_empty() || a.pattern.len() > 256 {
                    return Err("pattern must contain 1..256 bytes".into());
                }
                let regex = regex::RegexBuilder::new(&a.pattern)
                    .size_limit(1 << 20)
                    .dfa_size_limit(1 << 20)
                    .build()
                    .map_err(|_| "invalid or oversized Rust regex (no PCRE2)".to_string())?;
                if [&a.path, &a.include, &a.exclude]
                    .into_iter()
                    .flatten()
                    .any(|s| s.len() > 256)
                {
                    return Err("path/filter exceeds 256 bytes".into());
                }
                let mut files: BTreeMap<String, Vec<(usize, String)>> = BTreeMap::new();
                let mut returned = 0;
                let limit = 80.min(320usize.saturating_sub(self.evidence_lines));
                let mut matched = 0;
                for (path, text) in repo {
                    if a.path.as_ref().is_some_and(|p| {
                        !(p.is_empty()
                            || p == "."
                            || path == *p
                            || path.starts_with(&format!("{}/", p.trim_end_matches('/'))))
                    }) {
                        continue;
                    }
                    if a.include.as_ref().is_some_and(|p| !wildcard(p, &path))
                        || a.exclude.as_ref().is_some_and(|p| wildcard(p, &path))
                    {
                        continue;
                    }
                    for (i, line) in text.lines().enumerate() {
                        if regex.is_match(line) {
                            matched += 1;
                            if returned >= limit {
                                continue;
                            }
                            self.located.insert((path.clone(), i + 1));
                            files
                                .entry(path.clone())
                                .or_default()
                                .push((i + 1, line.into()));
                            returned += 1;
                        }
                    }
                }
                self.evidence_lines += returned;
                Ok(ToolResult {
                    files,
                    truncated: matched > limit,
                    bounded: false,
                })
            }
            "glob" => {
                let a: Glob = serde_json::from_str(&call.arguments).map_err(parse_error)?;
                if a.pattern.len() > 256 {
                    return Err("pattern too long".into());
                }
                Ok(ToolResult {
                    files: repo
                        .keys()
                        .filter(|p| wildcard(&a.pattern, p))
                        .map(|p| (p.clone(), Vec::new()))
                        .collect(),
                    truncated: false,
                    bounded: false,
                })
            }
            "read" => {
                let a: Range = serde_json::from_str(&call.arguments).map_err(parse_error)?;
                let text = repo.get(&a.path).ok_or("unknown virtual path")?;
                let eof = text.lines().count();
                if a.start_line == 0 || a.end_line < a.start_line || a.start_line > eof {
                    return Err("invalid line range".into());
                }
                let limit = 80.min(320usize.saturating_sub(self.evidence_lines));
                let last = a.end_line.min(eof).min(a.start_line - 1 + limit);
                let lines = text
                    .lines()
                    .enumerate()
                    .filter(|(i, _)| *i + 1 >= a.start_line && *i < last)
                    .map(|(i, t)| {
                        self.located.insert((a.path.clone(), i + 1));
                        (i + 1, t.to_string())
                    })
                    .collect::<Vec<_>>();
                self.evidence_lines += lines.len();
                Ok(ToolResult {
                    files: BTreeMap::from([(a.path, lines)]),
                    truncated: last < a.end_line,
                    bounded: last < a.end_line,
                })
            }
            _ => Err("unknown retrieval tool".into()),
        }
    }
}
fn wildcard(pattern: &str, path: &str) -> bool {
    let mut remaining = path;
    let parts = pattern.split('*').collect::<Vec<_>>();
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        let Some(at) = remaining.find(part) else {
            return false;
        };
        if i == 0 && at != 0 {
            return false;
        }
        remaining = &remaining[at + part.len()..];
    }
    pattern.ends_with('*') || remaining.is_empty()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    ranges: Vec<Range>,
}
fn lines(ranges: &[Range]) -> BTreeSet<(String, usize)> {
    ranges
        .iter()
        .flat_map(|r| (r.start_line..=r.end_line).map(|l| (r.path.clone(), l)))
        .collect()
}
fn metrics<T: Ord>(target: &BTreeSet<T>, predicted: &BTreeSet<T>) -> (f64, f64, f64) {
    let hit = target.intersection(predicted).count() as f64;
    let precision = if predicted.is_empty() {
        0.0
    } else {
        hit / predicted.len() as f64
    };
    let recall = if target.is_empty() {
        0.0
    } else {
        hit / target.len() as f64
    };
    let f = if precision + recall == 0.0 {
        0.0
    } else {
        1.25 * precision * recall / (0.25 * precision + recall)
    };
    (precision, recall, f)
}
fn parse_final(response: &str) -> Option<Answer> {
    serde_json::from_str::<Answer>(response).ok().filter(|a| {
        a.ranges.len() <= 16
            && a.ranges.iter().all(|r| {
                r.start_line > 0
                    && r.end_line >= r.start_line
                    && r.end_line <= 512
                    && !r.path.is_empty()
                    && r.path.len() <= 256
            })
    })
}

pub fn valid_final(response: &str) -> bool {
    parse_final(response).is_some()
}

pub fn grade(task: &Task, response: &str, located: &BTreeSet<(String, usize)>) -> Value {
    let answer = parse_final(response);
    let valid = answer.is_some();
    let ranges = answer.map(|a| a.ranges).unwrap_or_default();
    let target = lines(&task.targets);
    let predicted = lines(&ranges);
    let tf = target
        .iter()
        .map(|(p, _)| p.clone())
        .collect::<BTreeSet<_>>();
    let pf = predicted
        .iter()
        .map(|(p, _)| p.clone())
        .collect::<BTreeSet<_>>();
    let (fp, fr, ff) = metrics(&tf, &pf);
    let (lp, lr, lf) = metrics(&target, &predicted);
    json!({"target_files":tf,"predicted_files":pf,"file_precision":fp,"file_recall":fr,"file_f0_5":ff,"target_ranges":task.targets,"predicted_ranges":ranges,"line_precision":lp,"line_recall":lr,"line_f0_5":lf,"returned_lines":predicted.len(),"polluting_lines":predicted.difference(&target).count(),"grounded_success":valid&&target.is_subset(&predicted)&&target.is_subset(located),"final_format_valid":valid,"score":100.0*(0.5*ff+0.5*lf)})
}
