//! Synthetic public-to-native contract. The optional CPU harness executes the
//! unmodified, pinned NInfer parser/translator/renderer, never the Engine.
use std::io::Write;
use std::process::{Command, Stdio};

use scala_core::{EngineId, ModelId, ModelProfileId, ModelProfilesState, SettingsSchema};
use scala_engine::{EffectiveGenerationSettings, EngineAdapter, EngineError, InferenceMessage};
use scala_engine_ninfer::NinferAdapter;
use serde_json::{Value, json};

use crate::ninfer_protocol_contract::{RequestAdmission, backend_request};

fn admission() -> RequestAdmission {
    RequestAdmission {
        protocol_semantics: true,
        sampler_semantics: true,
        thinking_semantics: true,
        tool_calling: true,
        media_semantics: true,
        vision: false,
        greedy: false,
    }
}

fn native(body: &Value, template: &str) -> Option<Value> {
    let executable = std::env::var_os("SCALA_TEST_NINFER_CONTRACT_BIN")?;
    let source = std::path::PathBuf::from(std::env::var_os("SCALA_TEST_NINFER_SOURCE").unwrap());
    let mut child = Command::new(executable)
        .arg(source.join("tests/fixtures/frontend/thinking_toggle_chat_template.jinja"))
        .arg(source.join("tests/fixtures/frontend/reasoning_effort_chat_template.jinja"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            json!({"body": body, "template": template})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "CPU contract harness failed");
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

#[test]
fn unsloth_public_to_native_contract() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../scala-engine/testdata/named-chat-messages.json"
    ))
    .unwrap();
    let expected: Vec<InferenceMessage> =
        serde_json::from_value(fixture["normalized_messages"].clone()).unwrap();
    let mut profiles = ModelProfilesState::default();
    profiles
        .create(
            ModelProfileId::new("model").unwrap(),
            "Synthetic profile",
            ModelId("origin-neutral-artifact".into()),
            EngineId::new("ninfer").unwrap(),
        )
        .unwrap();
    let before_profiles = profiles.clone();
    let adapter = NinferAdapter::from_config(None, std::path::Path::new("."));

    for (scenario, length) in fixture["turns"].as_object().unwrap() {
        let length = length.as_u64().unwrap() as usize;
        for stream in [false, true] {
            let mut public = fixture["request"].clone();
            public["messages"].as_array_mut().unwrap().truncate(length);
            public["stream"] = json!(stream);
            let request = super::parse_request(public.clone())
                .unwrap()
                .normalized
                .inference_request()
                .unwrap();
            assert_eq!(request.messages, expected[..length], "{scenario}");
            // Exercise the real public alias -> Profile ID conversion and
            // profile binding lookup. No installed profiles or processes are used.
            let profile = &profiles.profiles[&request.model_profile_id];
            assert_eq!(profile.engine_id.as_str(), "ninfer");
            assert_eq!(profile.model_id.0, "origin-neutral-artifact");
            let validation = adapter.validate_inference_request(
                &request,
                &EffectiveGenerationSettings {
                    temperature: 0.0,
                    top_p: 1.0,
                },
                &SettingsSchema::default(),
            );
            let body = backend_request(&request, stream, admission());
            if request
                .messages
                .iter()
                .any(|message| message.name.is_some())
            {
                assert!(matches!(
                    validation,
                    Err(EngineError::MessageNamesUnsupported)
                ));
                assert!(matches!(body, Err(EngineError::MessageNamesUnsupported)));
                for template in ["thinking_toggle", "reasoning_effort"] {
                    if let Some(result) = native(&public, template) {
                        assert_eq!(result["error_code"], "message_name_not_supported");
                    }
                }
                continue;
            }
            validation.unwrap();
            let body = body.unwrap();
            assert_eq!(body["model"], profile.id.as_str());
            assert_eq!(body["stream"], stream);
            assert_eq!(
                body["messages"],
                json!(&fixture["native_messages"].as_array().unwrap()[..length])
            );
            for template in ["thinking_toggle", "reasoning_effort"] {
                if let Some(result) = native(&body, template) {
                    assert_native_messages(&body, &result);
                }
            }
        }
    }
    assert_eq!(profiles, before_profiles);

    // A client that never supplies participant names can replay every call and
    // result, including parallel calls. This is a separate input fixture, not a
    // compatibility normalization applied to a named request.
    let mut public = fixture["request"].clone();
    for message in public["messages"].as_array_mut().unwrap() {
        if message["role"] != "tool" {
            message.as_object_mut().unwrap().remove("name");
        }
    }
    public["tools"] = json!([
        {"type":"function","function":{"name":"web_search","parameters":{"type":"object","properties":{"query":{"type":"string"}}}}},
        {"type":"function","function":{"name":"edit_file","parameters":{"type":"object","properties":{"path":{"type":"string"},"edits":{"type":"array"}}}}}
    ]);
    public["tool_choice"] = json!("auto");
    public["parallel_tool_calls"] = json!(true);
    for stream in [false, true] {
        public["stream"] = json!(stream);
        let request = super::parse_request(public.clone())
            .unwrap()
            .normalized
            .inference_request()
            .unwrap();
        for template in ["thinking_toggle", "reasoning_effort"] {
            let body = backend_request(&request, stream, admission()).unwrap();
            assert_eq!(body["tool_choice"], "auto");
            assert_eq!(body["parallel_tool_calls"], true);
            assert_eq!(body["tools"][0]["function"]["name"], "web_search");
            assert_eq!(body["tools"][1]["function"]["name"], "edit_file");
            if let Some(result) = native(&body, template) {
                assert_native_messages(&body, &result);
            }
        }
        for unavailable in ["protocol", "tools"] {
            let mut proof = admission();
            match unavailable {
                "protocol" => proof.protocol_semantics = false,
                "tools" => proof.tool_calling = false,
                _ => unreachable!(),
            }
            assert!(backend_request(&request, stream, proof).is_err());
        }
    }
}

fn assert_native_messages(body: &Value, result: &Value) {
    let mut expected = body["messages"].clone();
    for message in expected.as_array_mut().unwrap() {
        if message["content"].is_null() {
            message["content"] = json!("");
        }
    }
    assert_eq!(result["messages"], expected);
    // Inspect only synthetic literals. Tool arguments are template JSON, not
    // user text; their wire strings need not retain whitespace after rendering.
    let rendered = result["rendered"].as_str().unwrap();
    let mut frontier = 0;
    for message in expected.as_array().unwrap() {
        let content = message["content"].as_str().unwrap();
        if !content.is_empty() {
            frontier += rendered[frontier..].find(content).unwrap() + content.len();
        }
    }
}

#[test]
fn unsloth_participant_identity_has_no_role_alias_exception() {
    for role in ["system", "developer", "user", "assistant"] {
        for name in [role, "web_search", "edit_file", "participant"] {
            let public = json!({"model":"model", "messages":[
                {"role":role,"name":name,"content":"synthetic identity content"}
            ]});
            let request = super::parse_request(public.clone())
                .unwrap()
                .normalized
                .inference_request()
                .unwrap();
            assert_eq!(request.messages[0].name.as_deref(), Some(name));
            assert_eq!(
                serde_json::to_value(request.messages[0].role).unwrap(),
                role
            );
            assert!(matches!(
                backend_request(&request, false, admission()),
                Err(EngineError::MessageNamesUnsupported)
            ));
            if let Some(result) = native(&public, "thinking_toggle") {
                assert_eq!(result["error_code"], "message_name_not_supported");
            }
        }
    }
    let public = json!({"model":"model", "messages":[{"role":"user","content":"synthetic"}],
        "chat_template_kwargs":{"participant_names":true}});
    assert!(super::parse_request(public.clone()).is_err());
    if let Some(result) = native(&public, "thinking_toggle") {
        assert_eq!(result["error_code"], "chat_template_option_not_supported");
    }
}

#[test]
fn unsloth_native_parallel_relationship_limit_is_explicit() {
    let body = json!({"model":"model","messages":[
        {"role":"user","content":"Synthetic parallel request"},
        {"role":"assistant","content":null,"tool_calls":[
            {"id":"first","type":"function","function":{"name":"web_search","arguments":"{}"}},
            {"id":"second","type":"function","function":{"name":"edit_file","arguments":"{}"}}
        ]},
        {"role":"tool","tool_call_id":"first","content":"Synthetic result A"},
        {"role":"tool","tool_call_id":"second","content":"Synthetic result B"}
    ]});
    for template in ["thinking_toggle", "reasoning_effort"] {
        let Some(original) = native(&body, template) else {
            return;
        };
        let mut differently_paired = body.clone();
        differently_paired["messages"][2]["tool_call_id"] = json!("second");
        differently_paired["messages"][3]["tool_call_id"] = json!("first");
        let changed = native(&differently_paired, template).unwrap();
        assert_ne!(original["messages"], changed["messages"]);
        // Native Chat retains IDs in its data types, but these two compiled
        // templates serialize only positional results. An ID-only change is
        // invisible to the model. This is a documented limitation, not a pass
        // for arbitrary OpenAI call/result permutations.
        assert_eq!(original["rendered"], changed["rendered"]);
    }
}
