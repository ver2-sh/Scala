//! Deterministic repository-style context; sizes are workload targets, not tokens.
use serde_json::{Value, json};
pub fn payload(target: usize, retrieval: bool) -> Value {
    let (instruction, facts, filler, answer) = if retrieval {
        (
            "Resolve the active GET /orders handler under CACHE_ENABLED=yes and AUTH_REQUIRED=yes. Follow its explicitly qualified callees, including the cache-miss fallback. Return ONLY a JSON array of the four implementation file paths in execution order (entry, authorization, cache, fallback). Ignore archived and UI-only definitions.\n",
            [
                "src/api/orders.rs: active GET /orders calls auth::gate::authorize when AUTH_REQUIRED=yes, then cache::orders::cached_orders when CACHE_ENABLED=yes.\n",
                "src/auth/gate.rs: active auth::gate::authorize checks the bearer credential and returns authorization; no other callees.\n",
                "src/cache/orders.rs: active cache::orders::cached_orders reads the cache, then calls store::orders::fetch_orders on a miss.\n",
                "src/store/orders.rs: active store::orders::fetch_orders reads persisted order rows; no other callees.\n",
            ],
            "archive/orders.rs: archived GET /orders calls legacy::fetch_orders.\nsrc/ui/orders.rs: UI-only ui::orders::cached_orders formats a badge; not an API handler or callee.\nsrc/legacy/gate.rs: archived authorize always succeeds.\n",
            json!([
                "src/api/orders.rs",
                "src/auth/gate.rs",
                "src/cache/orders.rs",
                "src/store/orders.rs"
            ]),
        )
    } else {
        (
            "Read the following repository deployment evidence. Only active records in region west contribute. Return ONLY a JSON array [sum of their workers, number with retries enabled, maximum revision]. Ignore archived records.\n",
            [
                "repo=alpha region=west active=true workers=7 retries=true revision=3\n",
                "repo=beta region=west active=true workers=5 retries=false revision=9\n",
                "repo=gamma region=west active=true workers=2 retries=true revision=6\n",
                "End of deployment evidence.\n",
            ],
            "repo=decoy region=east active=true workers=19 retries=true revision=99\nrepo=archived region=west active=false workers=41 retries=true revision=88\n",
            json!([14, 2, 9]),
        )
    };
    let mut text = instruction.to_owned();
    for (index, fact) in facts.iter().enumerate() {
        text.push_str(fact);
        if index < 3 {
            while text.len() < target * (index + 1) / 3 {
                text.push_str(filler);
            }
        }
    }
    text.push_str("Return only the requested JSON array; no explanation.\n");
    json!({"id":format!("ladder-{target}"),"target_workload_characters":target,"utf8_bytes":text.len(),"unicode_characters":text.chars().count(),"prompt":text,"answer":answer,"style":if retrieval{"repository call-chain evidence accumulation"}else{"structured context aggregation"}})
}
