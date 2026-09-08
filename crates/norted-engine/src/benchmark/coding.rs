//! Private original executable microtasks. No host capabilities are registered.
use rhai::packages::{
    ArithmeticPackage, BasicArrayPackage, BasicIteratorPackage, BasicMathPackage,
    BasicStringPackage, LogicPackage, MoreStringPackage, Package,
};
use rhai::{Dynamic, Engine, Scope};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Instant;

pub const RUBRIC: &str = "norted-rhai-all-hidden/1";
pub const SECONDS: u64 = 7;
pub const OUTPUT_TOKENS: u32 = 768;
const SYNTAX: &str = "Return only Rhai source defining fn solve(x) { ... }, no markdown. Rhai uses let v = 0; assignment v += 1; if condition { } else { }; for v in array { }; while condition { }; return value;. Arrays: [], a.len(), a[i], a.push(v); strings: s.len(), s[i] (character), s.split(\";\"), s.trim(), parse_int(s). Integer arithmetic and comparisons work normally; booleans true/false; no imports, IO, clocks, randomness or eval. x is the input described below. Return the specified value, not JSON text. All inputs are small (at most 64 elements, integer magnitudes below 10000).";
#[derive(Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub prompt: String,
    pub contract: String,
    pub seconds: u64,
    pub max_output_tokens: u32,
    pub rubric: String,
    pub tests: Vec<(Value, Value)>,
}
pub fn tasks() -> Vec<Task> {
    let rows = vec![
        (
            "clamped-prefix",
            "x = [values, cap], cap >= 0. Start balance at zero; for each signed integer in values add it, then clamp balance into [0, cap]. Return the balance after each item as an array.",
            vec![
                (json!([[], 4]), json!([])),
                (json!([[8, -3, -9, 2, 7], 5]), json!([5, 2, 0, 2, 5])),
                (json!([[1, -1, 3], 0]), json!([0, 0, 0])),
                (json!([[-2, 3, 2, -1], 4]), json!([0, 3, 4, 3])),
            ],
        ),
        (
            "boundary-repair",
            "Repair this boundary-bug specification: x = [sorted_values, target]. Return the index of the FIRST element >= target, or values.len() if none. Faulty approach initializes hi = len-1 and returns mid when equal, mishandling duplicates and empty arrays. Implement a correct function.",
            vec![
                (json!([[], 2]), json!(0)),
                (json!([[1, 3, 3, 3, 7], 3]), json!(1)),
                (json!([[1, 3], 4]), json!(2)),
                (json!([[-5, -2, 0], -9]), json!(0)),
                (json!([[2], 2]), json!(0)),
                (json!([[1, 4, 8], 5]), json!(2)),
            ],
        ),
        (
            "last-occurrence",
            "x is an integer array. Remove duplicates keeping each value's LAST occurrence; preserve the order of those retained occurrences. Return an array.",
            vec![
                (json!([]), json!([])),
                (json!([4, 2, 4, 3, 2]), json!([4, 3, 2])),
                (json!([0, 0, 0]), json!([0])),
                (json!([-1, 2, -1, 3, 2, 4]), json!([-1, 3, 2, 4])),
                (json!([3, 2, 1]), json!([3, 2, 1])),
            ],
        ),
        (
            "escaped-fields",
            "x is an ASCII string. Split at unescaped semicolons. A backslash escapes the next character (any character), and is removed; a trailing backslash is kept literally. Preserve empty fields, including at the end. Return the string array. Empty input returns one empty field.",
            vec![
                (json!(""), json!([""])),
                (json!("a;;b;"), json!(["a", "", "b", ""])),
                (json!(r"a\;b;c"), json!(["a;b", "c"])),
                (json!("end\\"), json!(["end\\"])),
                (json!(r"a\\;b\q"), json!(["a\\", "bq"])),
            ],
        ),
        (
            "explicit-precedence",
            "x = [runtime, layers]. runtime is an integer. Each layer is [present, value], with present boolean and value integer. Layers are ordered Settings, Profile, Load, Request. Only present=true overrides the current value; explicit zero and negative values win. Return [winning_value, source_index], where runtime source is 0 and layer sources are 1..4.",
            vec![
                (
                    json!([7, [[false, 9], [false, 9], [false, 9], [false, 9]]]),
                    json!([7, 0]),
                ),
                (
                    json!([7, [[true, 0], [false, 4], [false, 8], [false, 9]]]),
                    json!([0, 1]),
                ),
                (
                    json!([7, [[true, 7], [true, 7], [false, 0], [false, 0]]]),
                    json!([7, 2]),
                ),
                (
                    json!([2, [[true, 4], [true, 3], [true, -1], [true, 0]]]),
                    json!([0, 4]),
                ),
                (
                    json!([2, [[false, 0], [false, 0], [true, -3], [false, 0]]]),
                    json!([-3, 3]),
                ),
            ],
        ),
        (
            "interval-union",
            "x is an array of integer [start,end] half-open intervals, unsorted, with start <= end. Return the total length covered by their union. Ignore empty intervals and count overlap only once; touching endpoints do not add extra length.",
            vec![
                (json!([]), json!(0)),
                (json!([[1, 4], [2, 6], [8, 9]]), json!(6)),
                (json!([[5, 5], [-3, 0], [0, 2], [-2, 1]]), json!(5)),
                (json!([[5, 8], [1, 10], [2, 3], [1, 10]]), json!(9)),
                (json!([[4, 6], [0, 2]]), json!(4)),
            ],
        ),
    ];
    rows.into_iter()
        .map(|(id, contract, tests)| Task {
            id: format!("coding-{id}"),
            prompt: format!("{SYNTAX}\n{contract}"),
            contract: contract.into(),
            tests,
            seconds: SECONDS,
            max_output_tokens: OUTPUT_TOKENS,
            rubric: RUBRIC.into(),
        })
        .collect()
}
pub fn manifest() -> Value {
    json!({"tasks":tasks(),"evaluator":"rhai/1.26.0","rubric":RUBRIC,"source_bytes":8192,"operations_per_case":20000,"call_depth":8,"expression_depth":16,"variables":64,"items_per_array":256,"map_items":32,"bytes_per_string":2048,"evaluation_seconds":1,"capabilities":[],"packages":["ArithmeticPackage","LogicPackage","BasicStringPackage","MoreStringPackage","BasicArrayPackage","BasicIteratorPackage","BasicMathPackage"],"compile_optimization":"none","case_scope":"fresh engine and scope per hidden case"})
}
pub fn evaluate(task: &Task, source: &str) -> Value {
    let started = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if source.len() > 8192 {
            return (false, false, "source size limit");
        }
        for (input, expected) in &task.tests {
            // Deliberately exclude LanguageCorePackage (contains blocking sleep),
            // StandardPackage, CorePackage, function-pointer and JSON helpers.
            let mut engine = Engine::new_raw();
            engine.register_global_module(ArithmeticPackage::new().as_shared_module());
            engine.register_global_module(LogicPackage::new().as_shared_module());
            engine.register_global_module(BasicStringPackage::new().as_shared_module());
            engine.register_global_module(MoreStringPackage::new().as_shared_module());
            engine.register_global_module(BasicArrayPackage::new().as_shared_module());
            engine.register_global_module(BasicIteratorPackage::new().as_shared_module());
            engine.register_global_module(BasicMathPackage::new().as_shared_module());
            engine.set_optimization_level(rhai::OptimizationLevel::None);
            engine
                .set_max_operations(20000)
                .set_max_call_levels(8)
                .set_max_expr_depths(16, 16)
                .set_max_variables(64)
                .set_max_array_size(256)
                .set_max_map_size(32)
                .set_max_string_size(2048);
            for symbol in ["eval", "print", "debug", "sleep"] {
                engine.disable_symbol(symbol);
            }
            // No registration of host functions; no_module/no_time compiled in.
            engine.on_print(|_| {});
            engine.on_debug(|_, _, _| {});
            engine.on_progress(move |_| {
                (started.elapsed().as_secs_f64() >= 1.0).then_some(Dynamic::UNIT)
            });
            let ast = match engine.compile(source) {
                Ok(ast) => ast,
                Err(_) => return (false, false, "parse/compile failed"),
            };
            let input = match rhai::serde::to_dynamic(input) {
                Ok(v) => v,
                Err(_) => return (true, false, "evaluator input unavailable"),
            };
            let output = engine.call_fn::<Dynamic>(&mut Scope::new(), &ast, "solve", (input,));
            match output {
                Ok(value)
                    if rhai::serde::from_dynamic::<Value>(&value).is_ok_and(|v| v == *expected) => {
                }
                Ok(_) => return (true, false, "hidden test failed"),
                Err(_) => return (true, false, "execution failed or sandbox limit exceeded"),
            }
        }
        (true, true, "all hidden tests passed")
    }))
    .unwrap_or((false, false, "evaluator failure contained"));
    json!({"compiled":result.0,"passed":result.1,"reason":result.2,"evaluation_ms":started.elapsed().as_secs_f64()*1000.0,"rubric":RUBRIC,"evaluator":"rhai/1.26.0"})
}
