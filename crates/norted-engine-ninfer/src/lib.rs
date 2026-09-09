//! NInfer source runtime catalog, process adapter, and private protocol bridge.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use norted_core::{
    AcceleratorBinding, AcceleratorDevice, AcquisitionMethod, ArtifactFormat,
    ArtifactNativeIdentity, AvailableRuntime, EngineConfig, EngineInstallation, EngineRevision,
    HostCapabilities, InstalledRuntime, ModelArtifact, ModelProfileId, NinferArtifactIdentity,
    ResolvedSettings, RuntimeAcquisitionMethod, RuntimeCompatibility, RuntimeId,
    RuntimeProbeObservation, RuntimeRequirements, SettingValue, inspect_ninfer_container,
};
use norted_engine::{
    ApiCapability, BackendLoadPhase, BackendLoadProgress, CompatibilityDecision,
    EffectiveGenerationSettings, EngineAdapter, EngineCapabilities, EngineError, EngineFeature,
    EngineIdentity, EngineProbe, GenerationSettingsPatch, InferenceActivityReporter,
    InferenceOutput, InferenceRequest, InferenceStream, InstallationState, LaunchRequest,
    LaunchSpec, LoadProgressReporter, NativeOption, OptionValueKind, OutputFormat,
    PreparedModelInput, ProcessDescriptor, RuntimeVariantUpdateIdentity, UpdateState,
    capture_command, compatibility_for, isolated_cuda_environment, prepare_norted_package_input,
    prepare_norted_package_input_with_progress, revalidate_norted_package_before_launch,
    revalidate_norted_package_before_launch_with_progress, visible_nvidia_devices,
};
use reqwest::redirect::Policy;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use walkdir::WalkDir;

mod catalog;
mod protocol;
mod settings;

pub use catalog::NinferRuntimeCatalogProvider;

pub const ENGINE_ID: &str = "ninfer";
pub const UPSTREAM_REPOSITORY: &str = "https://github.com/Neroued/ninfer";
pub const GITHUB_REPOSITORY: &str = "Neroued/ninfer";
pub const PROVIDER_ID: &str = "ninfer-official-source";
const CURRENT_PACKAGE_CAPABILITY_REVISION: &str = "863aa8a5f1e866db74f29f8999b83b4021398dee";
const CURRENT_PACKAGE_CAPABILITY_TREE: &str = "5368f514bafbcab89ce1272df3a13d9d9af55820";
const LEGACY_PACKAGE_CAPABILITY_REVISION: &str = "a140e7ae82a11ed2f370a4d8f2cc16268a3790b8";
const LEGACY_PACKAGE_CAPABILITY_TREE: &str = "1474697c790de8df18ed07a469f560bb33e8f324";
const CURRENT_REQUEST_LOG_SCHEMA: u32 = 20;
const MANAGED_NINFER_FUNCTIONAL_VARIANT: &str = "managed-linux-x86_64-cuda-sm120a";

// These are Git blob identities, not whole-repository identities. A later
// upstream commit therefore retains a reviewed capability when the source
// files that own that capability are byte-for-byte unchanged.
#[rustfmt::skip]
const REVIEWED_SOURCE_BLOBS: &[(&str, &str)] = &[
    ("apps/cli/options.cpp", "f1c134cbef222c5c949bae5a368c31f17fbf4614"),
    ("include/ninfer/types.h", "d8398774a89c25126a0aae830340d946911c4df3"),
    ("src/product/media_acquire/acquire.cpp", "24644492e21122f225abcd7e2fc19718d1827d2c"),
    ("src/product/media_acquire/acquire.h", "91be5a244a0ff45e92acf498ce35afc404a07c89"),
    ("src/product/media_acquire/source.h", "8f9ed1c4ccb13b33a75bf7825f1002c61e4dcfb2"),
    ("src/product/speculative_options.h", "a50ea5fc8263e0f65f96861148a5276514cd4564"),
    ("src/runtime/contract/sampling.cpp", "495963c88468f215f63132ae901a7cbf455c1164"),
    ("src/runtime/contract/sampling.h", "8770f7f39208bf26da731ecd6e60b33ba8fe8a1e"),
    ("src/runtime/engine/engine.cpp", "ee15cf8ab8c961677bc8f8e27f0fab559f5b2f22"),
    ("src/runtime/engine/engine_core.h", "d4d9b2d678f21b0033009b873d2448e7c9d0b365"),
    ("src/runtime/engine/resource_manager.h", "f14837e23ea29ecb89541e1ebd916f1b60f788b2"),
    ("src/serve/generation_service.cpp", "686992f1017c73cc52a0043980ea4d52bea05d3e"),
    ("src/serve/generation_service.h", "8540d6ddc0b65b556d8c674167c10a63cf784e04"),
    ("src/serve/http_server.cpp", "e1c12dcd32881149656a894ef11319d4bc011d5e"),
    ("src/serve/http_server.h", "24314949fa23576a031550fba4c6e28e6f81e702"),
    ("src/serve/http_transport.cpp", "2840814b6dcd6c71e600d5ade553e14ce3dede77"),
    ("src/serve/http_transport.h", "22b68d45e208bcf52e14798516e9e949e76d4737"),
    ("src/serve/openai_chat.h", "c581acd7439cfee19acd39a5004b02d6f8f43747"),
    ("src/serve/openai_chat_http.cpp", "15ab4e6dbb52cf9d8f310cd2d05d729c5493776c"),
    ("src/serve/openai_chat_request.cpp", "2a7611c1e7178925d714fcadf591c9067caaef29"),
    ("src/serve/openai_chat_response.cpp", "5841d4b51b204307eca2fe4fb73edcd1dde8d73c"),
    ("src/serve/openai_common.cpp", "f16a90f61b984dd586b9bad1d63902d1baa23c31"),
    ("src/serve/openai_common.h", "1935327676b74a1716958f157bc9101376f93c11"),
    ("src/serve/openai_responses_store.cpp", "6fa1605b838cc5909902f3bb6a9d172f59b97595"),
    ("src/serve/openai_responses_store.h", "d8f68c3453b6b2e00b0dcddf45a591f379773469"),
    ("src/serve/request.h", "cf87d3621f573876cd47610ee4aceb8645785d0b"),
    ("src/serve/request_log.cpp", "b0dba5f2840e7cebc70cd868f57b66a89eaf38fa"),
    ("src/serve/request_log.h", "8e0f062394a3cb7a5d26097bcfcadc8128d11d41"),
    ("src/serve/request_json.h", "c61d5a1b1c8def6bac2a4f63a67febd451703121"),
    ("src/serve/request_validation.cpp", "9401ec3f136ce22e2c054ed16619703587ccd669"),
    ("src/serve/request_validation.h", "a5daabdaa99961573801fdeb8dd3cd58d91e496f"),
    ("src/serve/serve_options.cpp", "73c43a55a1102825d054c38730edcc2575489537"),
    ("src/serve/serve_options.h", "e147539590041b064cb1161e16c21e59749c1e76"),
    ("src/serve/translate.cpp", "c2effe323b25b5c1f8538ad43f4bc744984fcddc"),
    ("src/serve/translate.h", "5d56431f2d271a5a9beb69a87ab5141868b9dfb4"),
    ("src/targets/qwen3_6/impl/frontend/chat_template.cpp", "9fba17ab444808b45c2900fe9aeb7cda3b43fe16"),
    ("src/targets/qwen3_6/impl/frontend/chat_template.h", "a3be5d9881ca5820a5dabfc8cbf490a3173b0698"),
    ("src/targets/qwen3_6/impl/frontend/digest.cpp", "093051f81354e8d67e75d371e8669c5d4857deed"),
    ("src/targets/qwen3_6/impl/frontend/digest.h", "06a35ba0b0b3a261cfc10eb7a23a36a5315b8d78"),
    ("src/targets/qwen3_6/impl/frontend/frontend.cpp", "75c7138318a4571006b474ab3b189b0a189a0d16"),
    ("src/targets/qwen3_6/impl/frontend/media_cache.cpp", "1c3f383849ac97fa147f6c8f04f2a42934fa677e"),
    ("src/targets/qwen3_6/impl/frontend/media_cache.h", "37684cbce8ae066d2b0898bfd0603fa711bf3c85"),
    ("src/targets/qwen3_6/impl/frontend/processor.cpp", "e9823ee7465d222959425f700b0662bfaa72dfef"),
    ("src/targets/qwen3_6/impl/frontend/processor.h", "d96bb43a2e26422217851ef228a152e7db759eb4"),
    ("src/targets/qwen3_6/impl/frontend/resources.cpp", "460e30ec01f9e65bef7d5542cb48af536ce160eb"),
    ("src/targets/qwen3_6/impl/frontend/tool_call_parser.cpp", "38102c4a9c507d5b1b3296d940989138aa15408a"),
    ("src/targets/qwen3_6/impl/frontend/tool_call_parser.h", "4f7733ecdcbb63ec9347f6ecc52c55b10349e879"),
    ("src/runtime/engine/admission_policy.cpp", "36953259963bda808dd80b3d491cba5932260a8f"),
    ("src/runtime/engine/admission_policy.h", "a6c1045432df35aa33dbb1b3ff575b605964a014"),
    ("src/runtime/engine/context_cost.cpp", "fe0c885303b00945cc39add94bc0c32e99aded6d"),
    ("src/runtime/engine/context_cost.h", "3ecd66c933ca2dd752c0a61b132942488d1d116b"),
    ("src/runtime/engine/context_cost_defaults.cpp", "43ded5705a1af9f56035716c557e0ccba65ca172"),
    ("src/runtime/engine/context_portfolio_value.h", "ddac06c7ce9e3a60f09c34906379b608da614973"),
    ("src/runtime/engine/kv_capacity.cpp", "4870e77523bded9243f3624ed0b4eafddc76dcb6"),
    ("src/runtime/engine/kv_capacity.h", "65c5be7b788e1bdc4f8356602544877251ac106d"),
    ("src/runtime/engine/materialization_planner.h", "74e10a2a71c467371acb9222c74290d4779359fb"),
    ("src/runtime/engine/request_record.h", "4def8a6733dfd34ca188a8bbc25719bd6e92c1dd"),
    ("src/runtime/engine/resource_search.h", "08b14447dc9eebf1a5fbd3b2c3fe5737a5166938"),
    ("src/runtime/engine/scheduler.h", "38f2ab99da6151f510f4e1952b481b6b39a5bcb9"),
    ("src/runtime/engine/shared_capture_planner.h", "ade4f69d4e4df0f5b34bb7fed47af0a0809024dc"),
    ("src/targets/qwen3_6/export/ninfer/targets/qwen3_6/round_state.h", "c969ce32c6f41d28fd1f6982de6a4ecab72d0dbf"),
    ("src/targets/qwen3_6/impl/runtime/dflash_context.h", "ecb1dd47ce69275fe36d785657511b984d85d9cf"),
    ("src/targets/qwen3_6/impl/runtime/dflash_context_impl.h", "18b1935d4241227f2b706954ea5880c3244c8d7a"),
    ("src/targets/qwen3_6/impl/runtime/dflash_impl.h", "bce35899c9ade3cd2a92fe72907ed1a52e5dc347"),
    ("src/targets/qwen3_6/impl/runtime/host_kv_extent_store.h", "d3a90981d0ae959235da0887db231f6cff970c9b"),
    ("src/targets/qwen3_6/impl/runtime/layouts.h", "d72dbebf531a18bac97a58fa09951e014ce00d6f"),
    ("src/targets/qwen3_6/impl/runtime/layouts_impl.h", "b70437e3ec6d81a79beafa31707b53862b144d1c"),
    ("src/targets/qwen3_6/impl/runtime/logical_kv_store.h", "bfba64a9aa71c8558853c28c72f6e3431b632337"),
    ("src/targets/qwen3_6/impl/runtime/mtp_impl.h", "987e23ef828bb6078dd065ff9e2fe03792b3a035"),
    ("src/targets/qwen3_6/impl/runtime/pressure_planner.h", "729c25aaf22001adc9f8cc59b3e9fcb68cc632fd"),
    ("src/targets/qwen3_6/impl/runtime/program.h", "0ff969a3e4389994c7f7f6de0477597323e73387"),
    ("src/targets/qwen3_6/impl/runtime/program_impl.h", "9ef417b283a40dc1317c763ddf645c4f3b6cfa68"),
    ("src/targets/qwen3_6/impl/runtime/rebuild_work.h", "6f01b2e2f99e6a31ecde5954a90d6e0937a4a2b0"),
    ("src/targets/qwen3_6/impl/runtime/request_plan_impl.h", "5b7fe83b080530dc787c957e55ceac0f3c43d2b8"),
    ("src/targets/qwen3_6/impl/runtime/resource_projection.h", "86e3091ffe3d024ea0dd85b4088bccb608a20360"),
    ("src/targets/qwen3_6/impl/runtime/schedule.h", "c9dc6eebecdc945dfece1a154539169ab4f5e602"),
    ("src/targets/qwen3_6/impl/runtime/state_image_store.h", "a5b691307547b25dd50aa24240d3632158ca2109"),
    ("src/targets/qwen3_6/impl/runtime/text_context.h", "3b4deda322438d29a76e6d039e761c05553ca138"),
    ("src/targets/qwen3_6/impl/runtime/text_context_impl.h", "872c13531a95db62adf83e7047c821b5bd671ad3"),
    ("src/targets/qwen3_6/impl/runtime/text_prefill_impl.h", "3716872fc6e30a4ad0093aadef38140a9cffbe05"),
    ("src/targets/qwen3_6/impl/runtime/vision_context.h", "2e11abcfd96a2681b6f2bc2c52cf2b753a434fbf"),
    ("src/targets/qwen3_6/impl/runtime/vision_context_impl.h", "fd8214efb0962ae79f362ccd08a8365aa4c06ec8"),
    ("src/targets/qwen3_6/impl/runtime/vision_prefill.h", "3cb791af026b1a48d93b5eb06a87be835a76e65e"),
    ("src/targets/qwen3_6/impl/runtime/workspace_recipe.h", "576d049439020f040ea85766b2af0e3ff8edf75a"),
    ("src/targets/qwen3_6/impl/state/decoder_state.cpp", "cf9f9b3710caa78f5648176eb93922989f16d795"),
    ("src/targets/qwen3_6/impl/state/round_state.cpp", "eae86cbef5326af83919f4ad8fac944b016aaa8a"),
    ("src/targets/qwen3_6/impl/state/state_image.cpp", "91eabd0e102fc8dcd4eb5e352efae5ced8b0287d"),
    ("src/targets/qwen3_6_27b/impl/package.cpp", "c844d21eda2d5e93649247291491ac5deb32c4c8"),
    ("src/targets/qwen3_6_35b_a3b/impl/package.cpp", "15e55730e0296a1158e78db243ff94d1960040ef"),
];

const CORE_PROCESS_FILES: &[&str] = &[
    "apps/cli/options.cpp",
    "include/ninfer/types.h",
    "src/runtime/engine/admission_policy.cpp",
    "src/runtime/engine/admission_policy.h",
    "src/runtime/engine/engine.cpp",
    "src/runtime/engine/engine_core.h",
    "src/runtime/engine/scheduler.h",
    "src/serve/generation_service.cpp",
    "src/serve/generation_service.h",
    "src/serve/serve_options.cpp",
    "src/serve/serve_options.h",
    "src/targets/qwen3_6/export/ninfer/targets/qwen3_6/round_state.h",
    "src/targets/qwen3_6/impl/runtime/layouts.h",
    "src/targets/qwen3_6/impl/runtime/layouts_impl.h",
    "src/targets/qwen3_6/impl/runtime/program.h",
    "src/targets/qwen3_6/impl/runtime/program_impl.h",
];
const CONTEXT_CACHE_FILES: &[&str] = &[
    "include/ninfer/types.h",
    "src/runtime/engine/admission_policy.cpp",
    "src/runtime/engine/admission_policy.h",
    "src/runtime/engine/context_cost.cpp",
    "src/runtime/engine/context_cost.h",
    "src/runtime/engine/context_cost_defaults.cpp",
    "src/runtime/engine/context_portfolio_value.h",
    "src/runtime/engine/engine.cpp",
    "src/runtime/engine/engine_core.h",
    "src/runtime/engine/kv_capacity.cpp",
    "src/runtime/engine/kv_capacity.h",
    "src/runtime/engine/materialization_planner.h",
    "src/runtime/engine/resource_search.h",
    "src/runtime/engine/resource_manager.h",
    "src/runtime/engine/scheduler.h",
    "src/runtime/engine/shared_capture_planner.h",
    "src/serve/generation_service.cpp",
    "src/serve/generation_service.h",
    "src/serve/serve_options.cpp",
    "src/serve/serve_options.h",
    "src/targets/qwen3_6/export/ninfer/targets/qwen3_6/round_state.h",
    "src/targets/qwen3_6/impl/runtime/host_kv_extent_store.h",
    "src/targets/qwen3_6/impl/runtime/layouts.h",
    "src/targets/qwen3_6/impl/runtime/layouts_impl.h",
    "src/targets/qwen3_6/impl/runtime/logical_kv_store.h",
    "src/targets/qwen3_6/impl/runtime/pressure_planner.h",
    "src/targets/qwen3_6/impl/runtime/program.h",
    "src/targets/qwen3_6/impl/runtime/program_impl.h",
    "src/targets/qwen3_6/impl/runtime/rebuild_work.h",
    "src/targets/qwen3_6/impl/runtime/request_plan_impl.h",
    "src/targets/qwen3_6/impl/runtime/resource_projection.h",
    "src/targets/qwen3_6/impl/runtime/state_image_store.h",
    "src/targets/qwen3_6/impl/state/decoder_state.cpp",
    "src/targets/qwen3_6/impl/state/round_state.cpp",
    "src/targets/qwen3_6/impl/state/state_image.cpp",
];
const SPECULATION_FILES: &[&str] = &[
    "apps/cli/options.cpp",
    "include/ninfer/types.h",
    "src/product/speculative_options.h",
    "src/runtime/engine/engine.cpp",
    "src/serve/generation_service.cpp",
    "src/serve/generation_service.h",
    "src/serve/serve_options.cpp",
    "src/serve/serve_options.h",
    "src/targets/qwen3_6/export/ninfer/targets/qwen3_6/round_state.h",
    "src/targets/qwen3_6/impl/runtime/dflash_context.h",
    "src/targets/qwen3_6/impl/runtime/dflash_context_impl.h",
    "src/targets/qwen3_6/impl/runtime/dflash_impl.h",
    "src/targets/qwen3_6/impl/runtime/layouts.h",
    "src/targets/qwen3_6/impl/runtime/layouts_impl.h",
    "src/targets/qwen3_6/impl/runtime/mtp_impl.h",
    "src/targets/qwen3_6/impl/runtime/program.h",
    "src/targets/qwen3_6/impl/runtime/program_impl.h",
    "src/targets/qwen3_6/impl/runtime/request_plan_impl.h",
    "src/targets/qwen3_6/impl/runtime/resource_projection.h",
    "src/targets/qwen3_6/impl/runtime/schedule.h",
    "src/targets/qwen3_6/impl/runtime/text_prefill_impl.h",
    "src/targets/qwen3_6/impl/state/round_state.cpp",
    "src/targets/qwen3_6_27b/impl/package.cpp",
    "src/targets/qwen3_6_35b_a3b/impl/package.cpp",
];
const SERVING_LIMIT_FILES: &[&str] = &[
    "include/ninfer/types.h",
    "src/runtime/engine/admission_policy.cpp",
    "src/runtime/engine/admission_policy.h",
    "src/runtime/engine/engine_core.h",
    "src/runtime/engine/resource_manager.h",
    "src/runtime/engine/resource_search.h",
    "src/runtime/engine/scheduler.h",
    "src/serve/generation_service.cpp",
    "src/serve/generation_service.h",
    "src/serve/http_server.cpp",
    "src/serve/http_server.h",
    "src/serve/serve_options.cpp",
    "src/serve/serve_options.h",
    "src/targets/qwen3_6/impl/runtime/program.h",
    "src/targets/qwen3_6/impl/runtime/program_impl.h",
    "src/targets/qwen3_6/impl/runtime/request_plan_impl.h",
    "src/targets/qwen3_6/impl/runtime/resource_projection.h",
];
const RESPONSES_STORE_FILES: &[&str] = &[
    "src/serve/http_server.cpp",
    "src/serve/http_server.h",
    "src/serve/openai_responses_store.cpp",
    "src/serve/openai_responses_store.h",
    "src/serve/request.h",
    "src/serve/serve_options.cpp",
    "src/serve/serve_options.h",
];
const REQUEST_DEFAULT_FILES: &[&str] = &[
    "src/serve/openai_chat.h",
    "src/serve/openai_chat_http.cpp",
    "src/serve/openai_chat_request.cpp",
    "src/serve/request.h",
    "src/serve/request_json.h",
    "src/serve/request_validation.cpp",
    "src/serve/request_validation.h",
    "src/serve/serve_options.cpp",
    "src/serve/serve_options.h",
];
const PROCESS_SAMPLER_FILES: &[&str] = &[
    "include/ninfer/types.h",
    "src/runtime/contract/sampling.cpp",
    "src/runtime/contract/sampling.h",
    "src/serve/generation_service.cpp",
    "src/serve/generation_service.h",
    "src/serve/request.h",
    "src/serve/serve_options.cpp",
    "src/serve/serve_options.h",
    "src/serve/translate.cpp",
    "src/serve/translate.h",
];
const MODEL_SAMPLER_DEFAULT_FILES: &[&str] = &[
    "include/ninfer/types.h",
    "src/runtime/contract/sampling.cpp",
    "src/runtime/contract/sampling.h",
    "src/runtime/engine/engine.cpp",
    "src/targets/qwen3_6_27b/impl/package.cpp",
    "src/targets/qwen3_6_35b_a3b/impl/package.cpp",
];
const REQUEST_SAMPLER_FILES: &[&str] = &[
    "include/ninfer/types.h",
    "src/runtime/contract/sampling.cpp",
    "src/runtime/contract/sampling.h",
    "src/runtime/engine/engine.cpp",
    "src/serve/generation_service.cpp",
    "src/serve/generation_service.h",
    "src/serve/openai_chat.h",
    "src/serve/openai_chat_http.cpp",
    "src/serve/openai_chat_request.cpp",
    "src/serve/request.h",
    "src/serve/request_json.h",
    "src/serve/request_validation.cpp",
    "src/serve/request_validation.h",
    "src/serve/translate.cpp",
    "src/serve/translate.h",
];
const REQUEST_PROTOCOL_FILES: &[&str] = &[
    "include/ninfer/types.h",
    "src/serve/generation_service.cpp",
    "src/serve/generation_service.h",
    "src/serve/http_transport.cpp",
    "src/serve/http_transport.h",
    "src/serve/openai_chat.h",
    "src/serve/openai_chat_http.cpp",
    "src/serve/openai_chat_request.cpp",
    "src/serve/openai_chat_response.cpp",
    "src/serve/openai_common.cpp",
    "src/serve/openai_common.h",
    "src/serve/request.h",
    "src/serve/request_json.h",
    "src/serve/request_validation.cpp",
    "src/serve/request_validation.h",
    "src/serve/translate.cpp",
    "src/serve/translate.h",
    "src/targets/qwen3_6/impl/frontend/chat_template.cpp",
    "src/targets/qwen3_6/impl/frontend/chat_template.h",
    "src/targets/qwen3_6/impl/frontend/digest.cpp",
    "src/targets/qwen3_6/impl/frontend/digest.h",
    "src/targets/qwen3_6/impl/frontend/frontend.cpp",
];
const REQUEST_LOG_FILES: &[&str] = &["src/serve/request_log.cpp", "src/serve/request_log.h"];
const THINKING_PROCESS_FILES: &[&str] = &[
    "include/ninfer/types.h",
    "src/serve/generation_service.cpp",
    "src/serve/generation_service.h",
    "src/serve/request.h",
    "src/serve/serve_options.cpp",
    "src/serve/serve_options.h",
    "src/serve/translate.cpp",
    "src/serve/translate.h",
    "src/targets/qwen3_6/impl/frontend/chat_template.cpp",
    "src/targets/qwen3_6/impl/frontend/chat_template.h",
    "src/targets/qwen3_6/impl/frontend/digest.cpp",
    "src/targets/qwen3_6/impl/frontend/digest.h",
    "src/targets/qwen3_6/impl/frontend/frontend.cpp",
];
const THINKING_REQUEST_FILES: &[&str] = &[
    "include/ninfer/types.h",
    "src/serve/generation_service.cpp",
    "src/serve/generation_service.h",
    "src/serve/openai_chat.h",
    "src/serve/openai_chat_http.cpp",
    "src/serve/openai_chat_request.cpp",
    "src/serve/openai_chat_response.cpp",
    "src/serve/request.h",
    "src/serve/request_json.h",
    "src/serve/request_validation.cpp",
    "src/serve/request_validation.h",
    "src/serve/translate.cpp",
    "src/serve/translate.h",
    "src/targets/qwen3_6/impl/frontend/chat_template.cpp",
    "src/targets/qwen3_6/impl/frontend/chat_template.h",
    "src/targets/qwen3_6/impl/frontend/digest.cpp",
    "src/targets/qwen3_6/impl/frontend/digest.h",
    "src/targets/qwen3_6/impl/frontend/frontend.cpp",
];
const TOOL_CALLING_FILES: &[&str] = &[
    "include/ninfer/types.h",
    "src/serve/generation_service.cpp",
    "src/serve/generation_service.h",
    "src/serve/openai_chat.h",
    "src/serve/openai_chat_http.cpp",
    "src/serve/openai_chat_request.cpp",
    "src/serve/openai_chat_response.cpp",
    "src/serve/request.h",
    "src/serve/request_json.h",
    "src/serve/request_validation.cpp",
    "src/serve/request_validation.h",
    "src/serve/translate.cpp",
    "src/serve/translate.h",
    "src/targets/qwen3_6/impl/frontend/chat_template.cpp",
    "src/targets/qwen3_6/impl/frontend/chat_template.h",
    "src/targets/qwen3_6/impl/frontend/frontend.cpp",
    "src/targets/qwen3_6/impl/frontend/tool_call_parser.cpp",
    "src/targets/qwen3_6/impl/frontend/tool_call_parser.h",
];
const VISION_PROCESS_FILES: &[&str] = &[
    "apps/cli/options.cpp",
    "include/ninfer/types.h",
    "src/runtime/engine/engine.cpp",
    "src/serve/generation_service.cpp",
    "src/serve/generation_service.h",
    "src/serve/serve_options.cpp",
    "src/serve/serve_options.h",
    "src/targets/qwen3_6/impl/frontend/frontend.cpp",
    "src/targets/qwen3_6/impl/frontend/media_cache.cpp",
    "src/targets/qwen3_6/impl/frontend/media_cache.h",
    "src/targets/qwen3_6/impl/frontend/processor.cpp",
    "src/targets/qwen3_6/impl/frontend/processor.h",
    "src/targets/qwen3_6/export/ninfer/targets/qwen3_6/round_state.h",
    "src/targets/qwen3_6/impl/runtime/layouts.h",
    "src/targets/qwen3_6/impl/runtime/layouts_impl.h",
    "src/targets/qwen3_6/impl/runtime/program.h",
    "src/targets/qwen3_6/impl/runtime/program_impl.h",
    "src/targets/qwen3_6/impl/runtime/request_plan_impl.h",
    "src/targets/qwen3_6/impl/runtime/resource_projection.h",
    "src/targets/qwen3_6/impl/runtime/text_context.h",
    "src/targets/qwen3_6/impl/runtime/text_context_impl.h",
    "src/targets/qwen3_6/impl/runtime/text_prefill_impl.h",
    "src/targets/qwen3_6/impl/runtime/vision_context.h",
    "src/targets/qwen3_6/impl/runtime/vision_context_impl.h",
    "src/targets/qwen3_6/impl/runtime/vision_prefill.h",
    "src/targets/qwen3_6/impl/state/round_state.cpp",
    "src/targets/qwen3_6_27b/impl/package.cpp",
    "src/targets/qwen3_6_35b_a3b/impl/package.cpp",
];
const DFLASH_VISION_FILES: &[&str] = &[
    "apps/cli/options.cpp",
    "include/ninfer/types.h",
    "src/product/speculative_options.h",
    "src/serve/generation_service.cpp",
    "src/serve/generation_service.h",
    "src/serve/serve_options.cpp",
    "src/serve/serve_options.h",
    "src/targets/qwen3_6/export/ninfer/targets/qwen3_6/round_state.h",
    "src/targets/qwen3_6/impl/runtime/dflash_context.h",
    "src/targets/qwen3_6/impl/runtime/dflash_context_impl.h",
    "src/targets/qwen3_6/impl/runtime/dflash_impl.h",
    "src/targets/qwen3_6/impl/runtime/layouts.h",
    "src/targets/qwen3_6/impl/runtime/layouts_impl.h",
    "src/targets/qwen3_6/impl/runtime/program.h",
    "src/targets/qwen3_6/impl/runtime/program_impl.h",
    "src/targets/qwen3_6/impl/runtime/request_plan_impl.h",
    "src/targets/qwen3_6/impl/runtime/resource_projection.h",
    "src/targets/qwen3_6/impl/runtime/schedule.h",
    "src/targets/qwen3_6/impl/runtime/text_context.h",
    "src/targets/qwen3_6/impl/runtime/text_context_impl.h",
    "src/targets/qwen3_6/impl/runtime/text_prefill_impl.h",
    "src/targets/qwen3_6/impl/runtime/vision_context.h",
    "src/targets/qwen3_6/impl/runtime/vision_context_impl.h",
    "src/targets/qwen3_6/impl/runtime/vision_prefill.h",
    "src/targets/qwen3_6/impl/state/round_state.cpp",
    "src/targets/qwen3_6_35b_a3b/impl/package.cpp",
];
const MEDIA_REQUEST_FILES: &[&str] = &[
    "include/ninfer/types.h",
    "src/product/media_acquire/acquire.cpp",
    "src/product/media_acquire/acquire.h",
    "src/product/media_acquire/source.h",
    "src/serve/generation_service.cpp",
    "src/serve/generation_service.h",
    "src/serve/openai_chat.h",
    "src/serve/openai_chat_http.cpp",
    "src/serve/openai_chat_request.cpp",
    "src/serve/request.h",
    "src/serve/request_json.h",
    "src/serve/request_validation.cpp",
    "src/serve/request_validation.h",
    "src/serve/translate.cpp",
    "src/serve/translate.h",
    "src/targets/qwen3_6/impl/frontend/chat_template.cpp",
    "src/targets/qwen3_6/impl/frontend/chat_template.h",
    "src/targets/qwen3_6/impl/frontend/frontend.cpp",
    "src/targets/qwen3_6/impl/frontend/media_cache.cpp",
    "src/targets/qwen3_6/impl/frontend/media_cache.h",
    "src/targets/qwen3_6/impl/frontend/processor.cpp",
    "src/targets/qwen3_6/impl/frontend/processor.h",
    "src/targets/qwen3_6/impl/frontend/resources.cpp",
];

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const STARTUP_LOG_LIMIT: u64 = 512 * 1024;
const STARTUP_LOG_LINE_LIMIT: usize = 128 * 1024;

const MANAGED_ENVIRONMENT_VARIABLES: &[&str] = &["CUDA_VISIBLE_DEVICES"];

fn managed_ninfer_variant_update_identity(
    identity: &norted_core::RuntimeIdentity,
) -> Option<RuntimeVariantUpdateIdentity> {
    if identity.engine_id != ENGINE_ID
        || identity.package_family != catalog::PACKAGE_FAMILY
        || identity.platform != "linux"
        || identity.architecture != "x86_64"
        || identity.accelerator != "cuda"
        || identity.package.provider_id != PROVIDER_ID
        || identity.package.repository.as_deref() != Some(GITHUB_REPOSITORY)
    {
        return None;
    }
    let generation = match identity.variant.as_str() {
        "ninfer-serve-v1-sm120a" => 1,
        "ninfer-serve-v2-sm120a" => 2,
        "ninfer-serve-v3-sm120a" => 3,
        _ => return None,
    };
    Some(RuntimeVariantUpdateIdentity {
        functional_variant: MANAGED_NINFER_FUNCTIONAL_VARIANT.to_owned(),
        source_recipe_generation: Some(generation),
    })
}

// Norted owns identity, private transport, device selection, structured load
// settings, request semantics, and all sampler behavior.
const MANAGED_NATIVE_ARGUMENTS: &[&str] = &[
    "--host",
    "--port",
    "--api-key",
    "--model-id",
    "--max-context",
    "--kv-capacity",
    "--max-concurrency",
    "--max-pending-requests",
    "--pending-timeout-ms",
    "--prefill-chunk",
    "--log-stats-interval-ms",
    "--max-request-mib",
    "--device",
    "--request-log-jsonl",
    "--kv-dtype",
    "--spec",
    "--draft-tokens",
    "--lm-head-draft",
    "--no-cuda-graph",
    "--no-prefix-reuse",
    "--no-thinking",
    "--preserve-thinking",
    "--device-state-slots",
    "--host-state-slots",
    "--host-kv-mib",
    "--max-private-continuations",
    "--max-shared-prefixes",
    "--max-long-anchors-per-continuation",
    "--media-cache-mib",
    "--media-live-mib",
    "--media-preprocess-threads",
    "--response-store-max-records",
    "--response-store-max-mib",
    "--default-max-tokens",
    "--default-thinking-budget",
    "--vision",
    "--cors",
    "--temperature",
    "--top-p",
    "--top-k",
    "--min-p",
    "--presence-penalty",
    "--frequency-penalty",
    "--seed",
    "--greedy",
    "--context-cost-presets",
];

// NInfer currently has no raw native option whose ownership is both safe and
// outside the structured runtime-setting contract.
const ALLOWED_VALUE_NATIVE_ARGUMENTS: &[&str] = &[];

#[derive(Debug, Clone)]
struct PendingStartup {
    request_log_path: PathBuf,
    native_identity: NinferArtifactIdentity,
    public_model_id: ModelProfileId,
    accelerator: AcceleratorDevice,
    settings_requirements: Option<NinferStartupRequirements>,
    capabilities: NinferRuntimeCapabilities,
}

#[derive(Debug, Clone, PartialEq)]
struct NinferStartupRequirements {
    minimum_context_tokens: Option<u64>,
    max_concurrency: Option<u64>,
    kv_capacity_mode: Option<String>,
    kv_capacity: Option<u64>,
    kv_dtype: Option<String>,
    prefill_chunk: Option<u64>,
    cuda_graph: Option<bool>,
    prefix_reuse: Option<bool>,
    device_state_slots: Option<u64>,
    host_state_slots: Option<u64>,
    host_kv_bytes: Option<u64>,
    max_private_continuations: Option<u64>,
    max_shared_prefixes: Option<u64>,
    max_long_anchors_per_continuation: Option<u64>,
    speculative_backend: Option<String>,
    speculative_draft_window: Option<u64>,
    proposal_head: Option<String>,
    expected_thinking: Option<bool>,
    reconcile_reasoning_from_startup: bool,
    preserve_thinking: Option<bool>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<u64>,
    min_p: Option<f64>,
    presence_penalty: Option<f64>,
    frequency_penalty: Option<f64>,
    seed: Option<u64>,
    default_output_tokens: Option<u64>,
    default_thinking_budget: Option<u64>,
    max_pending_requests: Option<u64>,
    pending_timeout_ms: Option<u64>,
    log_stats_interval_ms: Option<u64>,
    max_request_bytes: Option<u64>,
    media_cache_bytes: Option<u64>,
    media_live_bytes: Option<u64>,
    media_preprocess_threads: Option<u64>,
    vision: Option<bool>,
    greedy: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default)]
struct NinferRuntimeCapabilities {
    trustworthy_identity: bool,
    core_process_controls: bool,
    context_cache_controls: bool,
    speculation_controls: bool,
    serving_limit_controls: bool,
    responses_store_controls: bool,
    request_default_controls: bool,
    process_sampler_controls: bool,
    model_sampler_defaults: bool,
    request_sampler_semantics: bool,
    request_protocol_semantics: bool,
    request_log_schema: Option<u32>,
    thinking_process_controls: bool,
    thinking_request_semantics: bool,
    tool_calling: bool,
    vision_process_controls: bool,
    dflash_vision: bool,
    media_request_semantics: bool,
    startup_proof: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct NinferReviewedDomains {
    core_process_controls: bool,
    context_cache_controls: bool,
    speculation_controls: bool,
    serving_limit_controls: bool,
    responses_store_controls: bool,
    request_default_controls: bool,
    process_sampler_controls: bool,
    model_sampler_defaults: bool,
    request_sampler_semantics: bool,
    request_protocol_semantics: bool,
    request_log: bool,
    thinking_process_controls: bool,
    thinking_request_semantics: bool,
    tool_calling: bool,
    vision_process_controls: bool,
    dflash_vision: bool,
    media_request_semantics: bool,
}

impl NinferReviewedDomains {
    fn all(reviewed: bool) -> Self {
        Self {
            core_process_controls: reviewed,
            context_cache_controls: reviewed,
            speculation_controls: reviewed,
            serving_limit_controls: reviewed,
            responses_store_controls: reviewed,
            request_default_controls: reviewed,
            process_sampler_controls: reviewed,
            model_sampler_defaults: reviewed,
            request_sampler_semantics: reviewed,
            request_protocol_semantics: reviewed,
            request_log: reviewed,
            thinking_process_controls: reviewed,
            thinking_request_semantics: reviewed,
            tool_calling: reviewed,
            vision_process_controls: reviewed,
            dflash_vision: reviewed,
            media_request_semantics: reviewed,
        }
    }
}

fn setting_capability_is_reviewed(id: &str, capabilities: NinferRuntimeCapabilities) -> bool {
    match id {
        "ninfer.stop_strings" | "ninfer.system_prompt" => capabilities.request_protocol_semantics,
        "ninfer.reasoning" | "ninfer.reasoning_effort" => {
            capabilities.request_protocol_semantics
                && capabilities.thinking_process_controls
                && capabilities.thinking_request_semantics
        }
        "ninfer.temperature"
        | "ninfer.top_p"
        | "ninfer.top_k"
        | "ninfer.min_p"
        | "ninfer.seed"
        | "ninfer.presence_penalty"
        | "ninfer.frequency_penalty"
        | "ninfer.greedy" => capabilities.process_sampler_controls,
        "ninfer.max_output_tokens" => capabilities.request_default_controls,
        "ninfer.reasoning_budget" | "ninfer.thinking" | "ninfer.preserve_thinking" => {
            capabilities.thinking_process_controls
        }
        "ninfer.context_length"
        | "ninfer.kv_dtype"
        | "ninfer.kv_capacity"
        | "ninfer.prefill_chunk"
        | "ninfer.prefix_reuse"
        | "ninfer.device_state_slots"
        | "ninfer.host_state_slots"
        | "ninfer.host_kv_mib"
        | "ninfer.max_private_continuations"
        | "ninfer.max_shared_prefixes"
        | "ninfer.max_long_anchors_per_continuation" => capabilities.context_cache_controls,
        "ninfer.cuda_graph" => capabilities.core_process_controls,
        "ninfer.speculation"
        | "ninfer.speculative_backend"
        | "ninfer.draft_tokens"
        | "ninfer.lm_head_draft" => capabilities.speculation_controls,
        "ninfer.parallel_requests"
        | "ninfer.max_pending_requests"
        | "ninfer.pending_timeout_ms"
        | "ninfer.log_stats_interval_ms"
        | "ninfer.max_request_mib" => capabilities.serving_limit_controls,
        "ninfer.cors" | "ninfer.log_level" => capabilities.core_process_controls,
        "ninfer.context_cost_presets" => capabilities.context_cache_controls,
        "ninfer.response_store_max_records" | "ninfer.response_store_max_mib" => {
            capabilities.responses_store_controls
        }
        "ninfer.vision"
        | "ninfer.media_cache_mib"
        | "ninfer.media_live_mib"
        | "ninfer.media_preprocess_threads" => capabilities.vision_process_controls,
        _ => false,
    }
}

fn apply_ninfer_runtime_contract(
    definitions: &mut [norted_core::SettingDefinition],
    help: &str,
    capabilities: NinferRuntimeCapabilities,
) {
    for definition in definitions.iter_mut() {
        let id = definition.id.as_str();
        let unsupported = |definition: &mut norted_core::SettingDefinition, reason: &str| {
            definition.supported = false;
            definition.unsupported_reason = Some(reason.to_owned());
        };
        if matches!(
            id,
            "ninfer.repeat_penalty"
                | "ninfer.reasoning_budget_message"
                | "ninfer.structured_output_schema"
                | "ninfer.context_overflow"
        ) {
            unsupported(
                definition,
                "NInfer does not implement this generation semantic",
            );
            continue;
        }
        if !setting_capability_is_reviewed(id, capabilities) {
            unsupported(
                definition,
                "this NInfer source runtime's capability contract for the setting is outdated or unreviewed; install or select the current reviewed runtime",
            );
            continue;
        }
        match settings::execution_path_for_setting(id) {
            settings::SettingExecutionPath::LaunchOption(option)
            | settings::SettingExecutionPath::VirtualLaunchControl(option)
                if !usage_has_token(help, option) =>
            {
                unsupported(
                    definition,
                    &format!("the exact ninfer-serve help contract does not advertise `{option}`"),
                );
            }
            settings::SettingExecutionPath::Unsupported => {
                unsupported(
                    definition,
                    "NInfer has no reviewed execution path for this setting",
                );
            }
            settings::SettingExecutionPath::LaunchOption(_)
            | settings::SettingExecutionPath::VirtualLaunchControl(_)
            | settings::SettingExecutionPath::NortedRequestDefault => {}
        }
    }
    if let Some(definition) = definitions
        .iter_mut()
        .find(|definition| definition.id.as_str() == "ninfer.kv_dtype")
        && let norted_core::SettingKind::Choice { choices } = &mut definition.kind
    {
        choices.retain(|choice| help.contains(choice));
    }
}

fn validate_ninfer_settings_prelaunch(
    settings: &ResolvedSettings,
    capabilities: NinferRuntimeCapabilities,
) -> Result<(), String> {
    if settings.is_empty() {
        return Ok(());
    }
    if !capabilities.trustworthy_identity {
        return Err(
            "the exact NInfer executable has no trustworthy capability observation; external binaries are not credited from filenames or version assumptions"
                .to_owned(),
        );
    }
    if let Some(id) = settings
        .configured
        .keys()
        .map(|id| id.as_str())
        .find(|id| !setting_capability_is_reviewed(id, capabilities))
    {
        return Err(format!(
            "NInfer capability contract for configured setting `{id}` is unsupported or unproven"
        ));
    }
    Ok(())
}

fn ninfer_runtime_capabilities_for_installed(
    runtime: &InstalledRuntime,
) -> NinferRuntimeCapabilities {
    let managed_source = runtime.manifest.acquisition_method
        == RuntimeAcquisitionMethod::SourceBuild
        && runtime.manifest.identity.package.repository.as_deref() == Some(GITHUB_REPOSITORY)
        && matches!(
            runtime.manifest.identity.variant.as_str(),
            "ninfer-serve-v2-sm120a" | "ninfer-serve-v3-sm120a"
        );
    let exact_current = managed_source
        && runtime.manifest.identity.upstream_revision.as_deref()
            == Some(CURRENT_PACKAGE_CAPABILITY_REVISION)
        && runtime.manifest.source_build.as_ref().is_some_and(|build| {
            build.source.commit_sha == CURRENT_PACKAGE_CAPABILITY_REVISION
                && build.source.tree_sha == CURRENT_PACKAGE_CAPABILITY_TREE
                && matches!(
                    build.recipe_version.as_str(),
                    "ninfer-serve-v2" | "ninfer-serve-v3"
                )
        });
    let exact_legacy = managed_source
        && runtime.manifest.identity.upstream_revision.as_deref()
            == Some(LEGACY_PACKAGE_CAPABILITY_REVISION)
        && runtime.manifest.source_build.as_ref().is_some_and(|build| {
            build.source.commit_sha == LEGACY_PACKAGE_CAPABILITY_REVISION
                && build.source.tree_sha == LEGACY_PACKAGE_CAPABILITY_TREE
                && matches!(
                    build.recipe_version.as_str(),
                    "ninfer-serve-v2" | "ninfer-serve-v3"
                )
        });
    let blobs = managed_source
        .then(|| installed_source_blobs(runtime))
        .flatten();
    let reviewed = |files| exact_current || source_domain_matches(blobs.as_ref(), files);
    let legacy_or_reviewed = |files| exact_legacy || reviewed(files);
    ninfer_reviewed_capabilities(
        managed_source,
        NinferReviewedDomains {
            core_process_controls: legacy_or_reviewed(CORE_PROCESS_FILES),
            context_cache_controls: legacy_or_reviewed(CONTEXT_CACHE_FILES),
            speculation_controls: legacy_or_reviewed(SPECULATION_FILES),
            serving_limit_controls: legacy_or_reviewed(SERVING_LIMIT_FILES),
            responses_store_controls: legacy_or_reviewed(RESPONSES_STORE_FILES),
            request_default_controls: legacy_or_reviewed(REQUEST_DEFAULT_FILES),
            process_sampler_controls: legacy_or_reviewed(PROCESS_SAMPLER_FILES),
            model_sampler_defaults: legacy_or_reviewed(MODEL_SAMPLER_DEFAULT_FILES),
            request_sampler_semantics: legacy_or_reviewed(REQUEST_SAMPLER_FILES),
            request_protocol_semantics: legacy_or_reviewed(REQUEST_PROTOCOL_FILES),
            request_log: legacy_or_reviewed(REQUEST_LOG_FILES),
            thinking_process_controls: legacy_or_reviewed(THINKING_PROCESS_FILES),
            thinking_request_semantics: legacy_or_reviewed(THINKING_REQUEST_FILES),
            tool_calling: legacy_or_reviewed(TOOL_CALLING_FILES),
            vision_process_controls: legacy_or_reviewed(VISION_PROCESS_FILES),
            dflash_vision: reviewed(DFLASH_VISION_FILES),
            media_request_semantics: legacy_or_reviewed(MEDIA_REQUEST_FILES),
        },
    )
}

fn ninfer_runtime_capabilities_for_available(
    runtime: &AvailableRuntime,
) -> NinferRuntimeCapabilities {
    let source = match &runtime.acquisition {
        norted_core::RuntimeAcquisitionPlan::SourceBuild(plan) => Some(&plan.source),
        _ => None,
    };
    let managed_source = source.is_some()
        && runtime.identity.package.repository.as_deref() == Some(GITHUB_REPOSITORY)
        && runtime.identity.variant == format!("{}-sm120a", catalog::RECIPE_VERSION);
    let current = managed_source
        && runtime.identity.upstream_revision.as_deref()
            == Some(CURRENT_PACKAGE_CAPABILITY_REVISION)
        && source.is_some_and(|source| {
            source.commit_sha == CURRENT_PACKAGE_CAPABILITY_REVISION
                && source.tree_sha == CURRENT_PACKAGE_CAPABILITY_TREE
        });
    ninfer_reviewed_capabilities(managed_source, NinferReviewedDomains::all(current))
}

fn ninfer_reviewed_capabilities(
    trustworthy_identity: bool,
    reviewed: NinferReviewedDomains,
) -> NinferRuntimeCapabilities {
    NinferRuntimeCapabilities {
        trustworthy_identity,
        core_process_controls: trustworthy_identity && reviewed.core_process_controls,
        context_cache_controls: trustworthy_identity && reviewed.context_cache_controls,
        speculation_controls: trustworthy_identity && reviewed.speculation_controls,
        serving_limit_controls: trustworthy_identity && reviewed.serving_limit_controls,
        responses_store_controls: trustworthy_identity && reviewed.responses_store_controls,
        request_default_controls: trustworthy_identity && reviewed.request_default_controls,
        process_sampler_controls: trustworthy_identity && reviewed.process_sampler_controls,
        model_sampler_defaults: trustworthy_identity && reviewed.model_sampler_defaults,
        request_sampler_semantics: trustworthy_identity && reviewed.request_sampler_semantics,
        request_protocol_semantics: trustworthy_identity && reviewed.request_protocol_semantics,
        request_log_schema: (trustworthy_identity && reviewed.request_log)
            .then_some(CURRENT_REQUEST_LOG_SCHEMA),
        thinking_process_controls: trustworthy_identity && reviewed.thinking_process_controls,
        thinking_request_semantics: trustworthy_identity && reviewed.thinking_request_semantics,
        tool_calling: trustworthy_identity && reviewed.tool_calling,
        vision_process_controls: trustworthy_identity && reviewed.vision_process_controls,
        dflash_vision: trustworthy_identity && reviewed.dflash_vision,
        media_request_semantics: trustworthy_identity && reviewed.media_request_semantics,
        startup_proof: trustworthy_identity && reviewed.request_log,
    }
}

fn installed_source_blobs(runtime: &InstalledRuntime) -> Option<BTreeMap<String, String>> {
    let source = runtime.manifest.source_build.as_ref()?;
    if !matches!(
        source.recipe_version.as_str(),
        "ninfer-serve-v2" | "ninfer-serve-v3"
    ) {
        return None;
    }
    let source_root = runtime.installation_root.join("source");
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(source_root)
        .arg("ls-tree")
        .arg("-r")
        .arg(&source.source.commit_sha)
        .arg("--");
    for (path, _) in REVIEWED_SOURCE_BLOBS {
        command.arg(path);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let output = String::from_utf8(output.stdout).ok()?;
    let mut blobs = BTreeMap::new();
    for line in output.lines() {
        let (metadata, path) = line.split_once('\t')?;
        let mut fields = metadata.split_whitespace();
        let _mode = fields.next()?;
        if fields.next()? != "blob" {
            return None;
        }
        let object = fields.next()?;
        if fields.next().is_some() {
            return None;
        }
        blobs.insert(path.to_owned(), object.to_owned());
    }
    Some(blobs)
}

fn source_domain_matches(observed: Option<&BTreeMap<String, String>>, required: &[&str]) -> bool {
    let Some(observed) = observed else {
        return false;
    };
    required.iter().all(|path| {
        let expected = REVIEWED_SOURCE_BLOBS
            .iter()
            .find_map(|(candidate, object)| (*candidate == *path).then_some(*object));
        expected
            .is_some_and(|expected| observed.get(*path).is_some_and(|actual| actual == expected))
    })
}

fn ninfer_startup_requirements(
    settings: &ResolvedSettings,
) -> Result<NinferStartupRequirements, EngineError> {
    let choice = |id: &str| match settings.value(id) {
        Some(SettingValue::Choice(value)) => Ok(Some(value.clone())),
        None => Ok(None),
        Some(_) => Err(EngineError::InvalidConfiguration(format!(
            "setting `{id}` must be a choice"
        ))),
    };
    let unsigned = |id: &str| match settings.value(id) {
        Some(SettingValue::UnsignedInteger(value)) => Ok(Some(*value)),
        None => Ok(None),
        Some(_) => Err(EngineError::InvalidConfiguration(format!(
            "setting `{id}` must be an unsigned integer"
        ))),
    };
    let toggle = |id: &str| match settings.value(id) {
        Some(SettingValue::Toggle(value)) => Ok(Some(*value)),
        None => Ok(None),
        Some(_) => Err(EngineError::InvalidConfiguration(format!(
            "setting `{id}` must be a toggle"
        ))),
    };
    let float = |id: &str| match settings.value(id) {
        Some(SettingValue::Float(value)) => Ok(Some(*value)),
        None => Ok(None),
        Some(_) => Err(EngineError::InvalidConfiguration(format!(
            "setting `{id}` must be a number"
        ))),
    };
    let integer = |id: &str| match settings.value(id) {
        Some(SettingValue::Integer(value)) => Ok(Some(*value)),
        None => Ok(None),
        Some(_) => Err(EngineError::InvalidConfiguration(format!(
            "setting `{id}` must be an integer"
        ))),
    };
    let seed = match settings.value("ninfer.seed") {
        Some(SettingValue::UnsignedIntegerOrChoice(
            norted_core::UnsignedIntegerOrChoiceValue::UnsignedInteger(value),
        )) => Some(*value),
        Some(SettingValue::UnsignedIntegerOrChoice(
            norted_core::UnsignedIntegerOrChoiceValue::Choice(value),
        )) if value == "random" => None,
        None => None,
        Some(_) => {
            return Err(EngineError::InvalidConfiguration(
                "setting `seed` must be an unsigned integer or `random`".to_owned(),
            ));
        }
    };
    let mib_bytes = |id: &str| -> Result<Option<u64>, EngineError> {
        unsigned(id)?
            .map(|value| {
                value.checked_mul(1_u64 << 20).ok_or_else(|| {
                    EngineError::InvalidConfiguration(format!("setting `{id}` is too large"))
                })
            })
            .transpose()
    };
    let speculation = toggle("ninfer.speculation")?;
    let backend = choice("ninfer.speculative_backend")?;
    let speculation_enabled = speculation.unwrap_or(backend.is_some());
    let default_thinking_budget = integer("ninfer.reasoning_budget")?
        .map(|value| {
            u64::try_from(value).map_err(|_| {
                EngineError::InvalidConfiguration(
                    "setting `reasoning_budget` must be non-negative".to_owned(),
                )
            })
        })
        .transpose()?;
    let (kv_capacity_mode, kv_capacity) = match settings.value("ninfer.kv_capacity") {
        Some(SettingValue::UnsignedIntegerOrChoice(
            norted_core::UnsignedIntegerOrChoiceValue::UnsignedInteger(value),
        )) => (Some("explicit".to_owned()), Some(*value)),
        Some(SettingValue::UnsignedIntegerOrChoice(
            norted_core::UnsignedIntegerOrChoiceValue::Choice(value),
        )) if value == "auto" => (Some("auto".to_owned()), None),
        None => (None, None),
        Some(_) => {
            return Err(EngineError::InvalidConfiguration(
                "setting `ninfer.kv_capacity` must be an unsigned integer or `auto`".to_owned(),
            ));
        }
    };
    Ok(NinferStartupRequirements {
        minimum_context_tokens: unsigned("ninfer.context_length")?,
        max_concurrency: unsigned("ninfer.parallel_requests")?,
        kv_capacity_mode,
        kv_capacity,
        kv_dtype: choice("ninfer.kv_dtype")?,
        prefill_chunk: unsigned("ninfer.prefill_chunk")?,
        cuda_graph: toggle("ninfer.cuda_graph")?,
        prefix_reuse: toggle("ninfer.prefix_reuse")?,
        device_state_slots: unsigned("ninfer.device_state_slots")?,
        host_state_slots: unsigned("ninfer.host_state_slots")?,
        host_kv_bytes: mib_bytes("ninfer.host_kv_mib")?,
        max_private_continuations: unsigned("ninfer.max_private_continuations")?,
        max_shared_prefixes: unsigned("ninfer.max_shared_prefixes")?,
        max_long_anchors_per_continuation: unsigned("ninfer.max_long_anchors_per_continuation")?,
        speculative_backend: if speculation_enabled {
            backend
        } else {
            Some("none".to_owned())
        },
        speculative_draft_window: if speculation_enabled {
            unsigned("ninfer.draft_tokens")?
        } else {
            Some(0)
        },
        proposal_head: toggle("ninfer.lm_head_draft")?
            .map(|enabled| if enabled { "optimized" } else { "full" }.to_owned()),
        expected_thinking: toggle("ninfer.thinking")?,
        reconcile_reasoning_from_startup: settings::reviewed_request_thinking_override(Some(
            settings,
        ))
        .is_none(),
        preserve_thinking: toggle("ninfer.preserve_thinking")?,
        temperature: float("ninfer.temperature")?,
        top_p: float("ninfer.top_p")?,
        top_k: unsigned("ninfer.top_k")?,
        min_p: float("ninfer.min_p")?,
        presence_penalty: float("ninfer.presence_penalty")?,
        frequency_penalty: float("ninfer.frequency_penalty")?,
        seed,
        default_output_tokens: unsigned("ninfer.max_output_tokens")?,
        default_thinking_budget,
        max_pending_requests: unsigned("ninfer.max_pending_requests")?,
        pending_timeout_ms: unsigned("ninfer.pending_timeout_ms")?,
        log_stats_interval_ms: unsigned("ninfer.log_stats_interval_ms")?,
        max_request_bytes: mib_bytes("ninfer.max_request_mib")?,
        media_cache_bytes: mib_bytes("ninfer.media_cache_mib")?,
        media_live_bytes: mib_bytes("ninfer.media_live_mib")?,
        media_preprocess_threads: unsigned("ninfer.media_preprocess_threads")?,
        vision: toggle("ninfer.vision")?,
        greedy: toggle("ninfer.greedy")?,
    })
}

pub struct NinferAdapter {
    enabled: bool,
    binary_path: Option<PathBuf>,
    native_arguments: Vec<String>,
    environment: BTreeMap<String, String>,
    configuration_error: Option<String>,
    client: reqwest::Client,
    capability_cache: tokio::sync::RwLock<BTreeMap<String, String>>,
    pending_startups: tokio::sync::RwLock<BTreeMap<String, PendingStartup>>,
    observed_defaults: tokio::sync::RwLock<BTreeMap<String, ObservedNinferStartup>>,
    active_executions: tokio::sync::RwLock<BTreeMap<String, NinferExecution>>,
}

#[derive(Debug, Clone, Copy)]
struct NinferExecution {
    capabilities: NinferRuntimeCapabilities,
    vision: bool,
    greedy: bool,
}

impl NinferAdapter {
    pub fn from_config(config: Option<&EngineConfig>, config_directory: &Path) -> Self {
        let mut enabled = config.is_none();
        let mut binary_path = None;
        let mut native_arguments = Vec::new();
        let mut environment = BTreeMap::new();
        let mut configuration_error = None;

        if let Some(config) = config {
            enabled = config.enabled;
            environment = config.env.clone();
            if let Some(name) = environment
                .keys()
                .find(|name| conflicts_with_managed_environment(name))
            {
                configuration_error = Some(format!(
                    "environment variable `{name}` conflicts with Norted's exact NInfer CUDA device isolation"
                ));
            }
            if let Some(key) = config.settings.keys().find(|key| *key != "binary_path") {
                configuration_error = Some(format!(
                    "unsupported NInfer setting `{key}`; only `binary_path` is supported"
                ));
            }
            if configuration_error.is_none()
                && let Some(value) = config.settings.get("binary_path")
            {
                match value.as_str() {
                    Some(value) if !value.trim().is_empty() => {
                        let configured = PathBuf::from(value);
                        binary_path = Some(if configured.is_absolute() {
                            configured
                        } else {
                            config_directory.join(configured)
                        });
                    }
                    _ => {
                        configuration_error =
                            Some("NInfer `binary_path` must be a non-empty string".to_owned());
                    }
                }
            }
            if let Some(key) = config.native.keys().find(|key| *key != "arguments") {
                configuration_error = Some(format!(
                    "unsupported NInfer native setting `{key}`; use `arguments = [...]`"
                ));
            }
            if configuration_error.is_none()
                && let Some(value) = config.native.get("arguments")
            {
                match value.as_array() {
                    Some(values) => {
                        for value in values {
                            match value.as_str() {
                                Some(value) if !value.contains('\0') => {
                                    native_arguments.push(value.to_owned());
                                }
                                Some(_) => {
                                    configuration_error = Some(
                                        "NInfer native arguments cannot contain NUL bytes"
                                            .to_owned(),
                                    );
                                    break;
                                }
                                None => {
                                    configuration_error = Some(
                                        "NInfer native arguments must all be strings".to_owned(),
                                    );
                                    break;
                                }
                            }
                        }
                    }
                    None => {
                        configuration_error = Some(
                            "NInfer native `arguments` must be an array of strings".to_owned(),
                        );
                    }
                }
            }
            if configuration_error.is_none()
                && let Some(argument) = native_arguments
                    .iter()
                    .find(|argument| conflicts_with_managed_argument(argument))
            {
                configuration_error = Some(format!(
                    "native argument `{argument}` conflicts with the Norted-managed NInfer contract"
                ));
            }
            if configuration_error.is_none()
                && let Some(argument) = invalid_native_argument(&native_arguments)
            {
                configuration_error = Some(format!(
                    "native argument `{argument}` is unsupported or has invalid arity; only documented non-semantic NInfer operational tuning options are accepted"
                ));
            }
        }

        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .build()
            .unwrap_or_else(|error| {
                configuration_error.get_or_insert_with(|| {
                    format!("could not create the private NInfer HTTP client: {error}")
                });
                reqwest::Client::new()
            });
        Self {
            enabled,
            binary_path,
            native_arguments,
            environment,
            configuration_error,
            client,
            capability_cache: tokio::sync::RwLock::new(BTreeMap::new()),
            pending_startups: tokio::sync::RwLock::new(BTreeMap::new()),
            observed_defaults: tokio::sync::RwLock::new(BTreeMap::new()),
            active_executions: tokio::sync::RwLock::new(BTreeMap::new()),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    async fn probe_uncached(&self) -> EngineProbe {
        if !self.enabled {
            return EngineProbe {
                installation: InstallationState::NotInstalled,
                update: UpdateState::Unknown,
                healthy: false,
                detail: "NInfer adapter is disabled in configuration".to_owned(),
            };
        }
        if let Some(error) = &self.configuration_error {
            return invalid_probe(error.clone());
        }
        let Some(configured_path) = &self.binary_path else {
            return EngineProbe {
                installation: InstallationState::NotInstalled,
                update: UpdateState::Unknown,
                healthy: false,
                detail: "no external ninfer-serve is configured; managed NInfer source runtimes remain available"
                    .to_owned(),
            };
        };
        let (binary_path, binary_sha256, observation) =
            match self.inspect_binary(configured_path, None).await {
                Ok(observation) => observation,
                Err(error) => return invalid_probe(error.to_string()),
            };
        EngineProbe {
            installation: InstallationState::Installed {
                installation: Box::new(EngineInstallation {
                    engine: EngineRevision {
                        engine_id: ENGINE_ID.to_owned(),
                        version: None,
                        revision: None,
                    },
                    source_repository: None,
                    acquisition_method: AcquisitionMethod::ExternalBinary,
                    binary_path,
                    binary_sha256: Some(binary_sha256),
                    build: None,
                    platform: std::env::consts::OS.to_owned(),
                    architecture: std::env::consts::ARCH.to_owned(),
                    runtime_variant: Some("external-binary".to_owned()),
                    acquired_at_unix: None,
                    observed_at_unix: observation.observed_at_unix,
                }),
            },
            update: UpdateState::Unknown,
            healthy: true,
            detail: format!("external configured ninfer-serve; {}", observation.detail),
        }
    }

    async fn inspect_binary(
        &self,
        configured_path: &Path,
        expected_sha256: Option<&str>,
    ) -> Result<(PathBuf, String, RuntimeProbeObservation), EngineError> {
        let binary_path =
            canonical_regular_file(configured_path, "ninfer-serve entrypoint").await?;
        let binary_sha256 = hash_file(&binary_path).await.map_err(|error| {
            EngineError::Operation(format!("could not hash entrypoint: {error}"))
        })?;
        if let Some(expected) = expected_sha256
            && !binary_sha256.eq_ignore_ascii_case(expected)
        {
            return Err(EngineError::InvalidConfiguration(format!(
                "runtime entrypoint SHA-256 mismatch: expected {expected}, observed {binary_sha256}"
            )));
        }
        let cached = self
            .capability_cache
            .read()
            .await
            .contains_key(&binary_sha256);
        if !cached {
            let output = capture_command(
                &binary_path,
                &["--help"],
                &self.environment,
                &managed_environment_removals(),
                PROBE_TIMEOUT,
            )
            .await?;
            let help = command_detail(&output.stdout, &output.stderr);
            if !output.success {
                return Err(EngineError::InvalidConfiguration(format!(
                    "ninfer-serve --help failed with exit code {:?}: {help}",
                    output.code
                )));
            }
            if let Some(reason) = help_contract_error(&help, &self.native_arguments) {
                return Err(EngineError::InvalidConfiguration(format!(
                    "entrypoint does not satisfy the ninfer-serve launch contract ({reason}): {help}"
                )));
            }
            self.capability_cache
                .write()
                .await
                .insert(binary_sha256.clone(), help);
        }
        Ok((
            binary_path,
            binary_sha256,
            RuntimeProbeObservation {
                compatible: true,
                observed_engine_id: ENGINE_ID.to_owned(),
                observed_version: None,
                observed_revision: None,
                detail: "ninfer-serve help contract recognized; executable reports no version or revision"
                    .to_owned(),
                observed_at_unix: unix_timestamp(),
            },
        ))
    }
}

#[async_trait]
impl EngineAdapter for NinferAdapter {
    fn identity(&self) -> EngineIdentity {
        EngineIdentity {
            id: ENGINE_ID.to_owned(),
            display_name: "NInfer".to_owned(),
            upstream_repository: UPSTREAM_REPOSITORY.to_owned(),
        }
    }

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            artifact_formats: vec![ArtifactFormat::Ninfer],
            api: vec![ApiCapability::Responses, ApiCapability::ChatCompletions],
            features: vec![
                EngineFeature::TextGeneration,
                EngineFeature::ToolCalling,
                EngineFeature::Vision,
            ],
        }
    }

    fn serving_features(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        settings: Option<&ResolvedSettings>,
    ) -> Vec<EngineFeature> {
        let capabilities = ninfer_runtime_capabilities_for_installed(runtime);
        let mut features = vec![EngineFeature::TextGeneration];
        if capabilities.tool_calling {
            features.push(EngineFeature::ToolCalling);
        }
        if capabilities.vision_process_controls
            && capabilities.media_request_semantics
            && matches!(
                model.native_identity,
                Some(ArtifactNativeIdentity::Ninfer(_))
            )
            && settings.is_some_and(|settings| {
                matches!(
                    settings.value("ninfer.vision"),
                    Some(SettingValue::Toggle(true))
                )
            })
        {
            features.push(EngineFeature::Vision);
        }
        features
    }

    fn uses_setting_as_request_default(&self, id: &str) -> bool {
        id != "reasoning_budget"
    }

    fn runtime_variant_update_identity(
        &self,
        identity: &norted_core::RuntimeIdentity,
    ) -> RuntimeVariantUpdateIdentity {
        managed_ninfer_variant_update_identity(identity)
            .unwrap_or_else(|| RuntimeVariantUpdateIdentity::exact(identity))
    }

    fn runtime_management_compatibility(&self) -> CompatibilityDecision {
        if !self.enabled {
            CompatibilityDecision::Unsupported {
                reason: "NInfer adapter is disabled in configuration".to_owned(),
            }
        } else if let Some(error) = &self.configuration_error {
            CompatibilityDecision::Unsupported {
                reason: error.clone(),
            }
        } else {
            CompatibilityDecision::Supported
        }
    }

    fn available_runtime_compatibility(&self, runtime: &AvailableRuntime) -> CompatibilityDecision {
        if runtime.identity.engine_id != ENGINE_ID {
            return CompatibilityDecision::Unsupported {
                reason: format!(
                    "runtime belongs to engine `{}`, not `{ENGINE_ID}`",
                    runtime.identity.engine_id
                ),
            };
        }
        if !runtime.supported_formats.contains(&ArtifactFormat::Ninfer) {
            return CompatibilityDecision::Unsupported {
                reason: "runtime does not declare NInfer artifact support".to_owned(),
            };
        }
        CompatibilityDecision::Supported
    }

    fn runtime_compatibility(&self, runtime: &InstalledRuntime) -> CompatibilityDecision {
        if runtime.manifest.identity.engine_id != ENGINE_ID {
            return CompatibilityDecision::Unsupported {
                reason: format!(
                    "runtime belongs to engine `{}`, not `{ENGINE_ID}`",
                    runtime.manifest.identity.engine_id
                ),
            };
        }
        if !runtime
            .manifest
            .supported_formats
            .contains(&ArtifactFormat::Ninfer)
        {
            return CompatibilityDecision::Unsupported {
                reason: "runtime does not declare NInfer artifact support".to_owned(),
            };
        }
        CompatibilityDecision::Supported
    }

    fn compatibility(&self, model: &ModelArtifact) -> CompatibilityDecision {
        if let CompatibilityDecision::Unsupported { reason } =
            self.runtime_management_compatibility()
        {
            return CompatibilityDecision::Unsupported { reason };
        }
        if model.format != ArtifactFormat::Ninfer {
            return CompatibilityDecision::Unsupported {
                reason: format!(
                    "NInfer requires a `.ninfer` artifact, not `{}`",
                    model.format.as_str()
                ),
            };
        }
        match model.native_identity.as_ref() {
            Some(ArtifactNativeIdentity::Ninfer(identity)) if identity.container_version == 2 => {
                CompatibilityDecision::Supported
            }
            Some(ArtifactNativeIdentity::Ninfer(identity)) => CompatibilityDecision::Unsupported {
                reason: format!(
                    "NInfer container version {} is unsupported; version 2 is required",
                    identity.container_version
                ),
            },
            Some(ArtifactNativeIdentity::Gguf(_)) => CompatibilityDecision::Unsupported {
                reason: "NInfer model identity is GGUF, not a NInfer container".to_owned(),
            },
            None => CompatibilityDecision::Unsupported {
                reason: "NInfer model is missing inspected native container identity; rediscover the artifact"
                    .to_owned(),
            },
        }
    }

    fn runtime_model_compatibility(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
        _settings: Option<&ResolvedSettings>,
    ) -> RuntimeCompatibility {
        if !model.generation_contract.required_stop_token_ids.is_empty() {
            let mut definitions = settings::definitions();
            apply_ninfer_stop_token_contract(&mut definitions, runtime, _settings);
            if !definitions
                .iter()
                .any(|d| d.id.as_str() == "ninfer.stop_token_ids" && d.supported)
            {
                return RuntimeCompatibility::Incompatible("artifact requires exact token stops; this NInfer runtime needs the reviewed Norted overlay and speculation off".to_owned());
            }
        }
        if let CompatibilityDecision::Unsupported { reason } = self.compatibility(model) {
            return RuntimeCompatibility::Incompatible(reason);
        }
        if let CompatibilityDecision::Unsupported { reason } = self.runtime_compatibility(runtime) {
            return RuntimeCompatibility::Incompatible(reason);
        }
        let capabilities = ninfer_runtime_capabilities_for_installed(runtime);
        let native = native_compatibility(
            &runtime.manifest.supported_native_identities,
            model
                .native_identity
                .as_ref()
                .expect("compatibility checked identity"),
        );
        let device = ninfer_device_evaluation(
            &runtime.manifest.identity.platform,
            &runtime.manifest.identity.architecture,
            &runtime.manifest.requirements,
            host,
            runtime.manifest.acquisition_method == RuntimeAcquisitionMethod::ExternalBinary,
        )
        .compatibility;
        let base = combine_compatibility(native, device);
        if runtime.manifest.acquisition_method == RuntimeAcquisitionMethod::SourceBuild
            && (!capabilities.trustworthy_identity
                || !capabilities.request_protocol_semantics
                || !capabilities.request_sampler_semantics
                || !capabilities.startup_proof)
        {
            combine_compatibility(
                base,
                RuntimeCompatibility::NeedsAttention(
                    "this NInfer source runtime has outdated or unreviewed request/request-sampler/startup capability contracts; install or select the current reviewed runtime"
                        .to_owned(),
                ),
            )
        } else {
            base
        }
    }

    fn available_runtime_model_compatibility(
        &self,
        runtime: &AvailableRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
        _settings: Option<&ResolvedSettings>,
    ) -> RuntimeCompatibility {
        if let CompatibilityDecision::Unsupported { reason } = self.compatibility(model) {
            return RuntimeCompatibility::Incompatible(reason);
        }
        if let CompatibilityDecision::Unsupported { reason } =
            self.available_runtime_compatibility(runtime)
        {
            return RuntimeCompatibility::Incompatible(reason);
        }
        let capabilities = ninfer_runtime_capabilities_for_available(runtime);
        let native = native_compatibility(
            &runtime.supported_native_identities,
            model
                .native_identity
                .as_ref()
                .expect("compatibility checked identity"),
        );
        let device = ninfer_device_evaluation(
            &runtime.identity.platform,
            &runtime.identity.architecture,
            &runtime.requirements,
            host,
            false,
        )
        .compatibility;
        let base = combine_compatibility(native, device);
        if matches!(
            &runtime.acquisition,
            norted_core::RuntimeAcquisitionPlan::SourceBuild(_)
        ) && (!capabilities.trustworthy_identity
            || !capabilities.request_protocol_semantics
            || !capabilities.request_sampler_semantics
            || !capabilities.startup_proof)
        {
            combine_compatibility(
                base,
                RuntimeCompatibility::NeedsAttention(
                    "this NInfer source runtime has outdated or unreviewed request/request-sampler/startup capability contracts; install or select the current reviewed runtime"
                        .to_owned(),
                ),
            )
        } else {
            base
        }
    }

    fn validate_configuration(
        &self,
        runtime: &InstalledRuntime,
        model: Option<&ModelArtifact>,
        _host: &HostCapabilities,
        resolved: &ResolvedSettings,
    ) -> Result<(), EngineError> {
        let capabilities = ninfer_runtime_capabilities_for_installed(runtime);
        validate_ninfer_settings_prelaunch(resolved, capabilities)
            .map_err(EngineError::InvalidConfiguration)?;
        if let Some(model) = model {
            settings::validate_model_settings(resolved, model, capabilities.dflash_vision)
                .map_err(EngineError::InvalidConfiguration)?;
        }
        Ok(())
    }

    fn runtime_model_accelerator_binding(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        host: &HostCapabilities,
        _settings: Option<&norted_core::ResolvedSettings>,
    ) -> Option<AcceleratorBinding> {
        matches!(self.compatibility(model), CompatibilityDecision::Supported).then(|| {
            ninfer_device_evaluation(
                &runtime.manifest.identity.platform,
                &runtime.manifest.identity.architecture,
                &runtime.manifest.requirements,
                host,
                runtime.manifest.acquisition_method == RuntimeAcquisitionMethod::ExternalBinary,
            )
            .accelerator
            .map(AcceleratorBinding::single)
        })?
    }

    async fn prepare_model_input(
        &self,
        model: &ModelArtifact,
    ) -> Result<PreparedModelInput, EngineError> {
        if model.format != ArtifactFormat::Ninfer {
            return Err(EngineError::InvalidConfiguration(
                "NInfer can only prepare `.ninfer` artifacts".to_owned(),
            ));
        }
        let expected = model.native_identity.clone().ok_or_else(|| {
            EngineError::InvalidConfiguration(
                "NInfer artifact is missing its inspected native identity".to_owned(),
            )
        })?;
        let canonical = canonical_regular_file(&model.path, "NInfer model").await?;
        let inspected = inspect_ninfer_container(&canonical).map_err(|error| {
            EngineError::InvalidConfiguration(format!(
                "could not re-inspect NInfer artifact {}: {error}",
                canonical.display()
            ))
        })?;
        let observed = ArtifactNativeIdentity::Ninfer(inspected.identity);
        if observed != expected {
            return Err(EngineError::InvalidConfiguration(
                "NInfer artifact native identity changed after discovery".to_owned(),
            ));
        }
        if model.norted_package.is_some() {
            let mut prepared = prepare_norted_package_input(model).await?;
            prepared.primary.native_identity = Some(observed);
            Ok(prepared)
        } else {
            let mut primary = model.clone();
            primary.path = canonical;
            primary.native_identity = Some(observed);
            Ok(PreparedModelInput {
                primary,
                auxiliary: Vec::new(),
                primary_file_identity: None,
            })
        }
    }

    async fn prepare_model_input_with_progress(
        &self,
        model: &ModelArtifact,
        progress: LoadProgressReporter,
    ) -> Result<PreparedModelInput, EngineError> {
        if model.format != ArtifactFormat::Ninfer {
            return Err(EngineError::InvalidConfiguration(
                "NInfer can only prepare `.ninfer` artifacts".to_owned(),
            ));
        }
        let expected = model.native_identity.clone().ok_or_else(|| {
            EngineError::InvalidConfiguration(
                "NInfer artifact is missing its inspected native identity".to_owned(),
            )
        })?;
        let canonical = canonical_regular_file(&model.path, "NInfer model").await?;
        let inspected = inspect_ninfer_container(&canonical).map_err(|error| {
            EngineError::InvalidConfiguration(format!(
                "could not re-inspect NInfer artifact {}: {error}",
                canonical.display()
            ))
        })?;
        let observed = ArtifactNativeIdentity::Ninfer(inspected.identity);
        if observed != expected {
            return Err(EngineError::InvalidConfiguration(
                "NInfer artifact native identity changed after discovery".to_owned(),
            ));
        }
        if model.norted_package.is_some() {
            let mut prepared = prepare_norted_package_input_with_progress(model, &progress).await?;
            prepared.primary.native_identity = Some(observed);
            Ok(prepared)
        } else {
            let mut primary = model.clone();
            primary.path = canonical;
            primary.native_identity = Some(observed);
            Ok(PreparedModelInput {
                primary,
                auxiliary: Vec::new(),
                primary_file_identity: None,
            })
        }
    }

    fn native_options(&self) -> Vec<NativeOption> {
        vec![NativeOption {
            name: "arguments".to_owned(),
            description: "Reserved NInfer native arguments; no raw options are currently allowed"
                .to_owned(),
            value_kind: OptionValueKind::String,
            repeatable: true,
        }]
    }

    fn setting_definitions(&self) -> Vec<norted_core::SettingDefinition> {
        settings::definitions()
    }

    fn model_setting_definitions(
        &self,
        model: &ModelArtifact,
    ) -> Result<Vec<norted_core::SettingDefinition>, EngineError> {
        let mut definitions = settings::definitions();
        settings::apply_model_capabilities(&mut definitions, model, None, false);
        Ok(definitions)
    }

    async fn runtime_settings_schema(
        &self,
        runtime: &InstalledRuntime,
        _host: &HostCapabilities,
        settings: Option<&ResolvedSettings>,
    ) -> Result<norted_core::SettingsSchema, EngineError> {
        self.probe_runtime(runtime).await?;
        let help = self
            .capability_cache
            .read()
            .await
            .get(&runtime.manifest.entrypoint_sha256.to_ascii_lowercase())
            .cloned()
            .ok_or_else(|| {
                EngineError::Operation("NInfer help observation was not cached".to_owned())
            })?;
        let mut definitions = settings::definitions();
        settings::apply_runtime_bounds(&mut definitions);
        let capabilities = ninfer_runtime_capabilities_for_installed(runtime);
        if capabilities.trustworthy_identity {
            settings::apply_reviewed_runtime_defaults(&mut definitions, settings);
        }
        apply_ninfer_runtime_contract(&mut definitions, &help, capabilities);
        apply_ninfer_stop_token_contract(&mut definitions, runtime, settings);
        Ok(norted_core::SettingsSchema {
            engine_id: ENGINE_ID.to_owned(),
            runtime_id: Some(runtime.manifest.runtime_id.clone()),
            definitions,
        })
    }

    async fn settings_schema(
        &self,
        runtime: &InstalledRuntime,
        model: &ModelArtifact,
        _host: &HostCapabilities,
        settings: Option<&ResolvedSettings>,
    ) -> Result<norted_core::SettingsSchema, EngineError> {
        self.probe_runtime(runtime).await?;
        let help = self
            .capability_cache
            .read()
            .await
            .get(&runtime.manifest.entrypoint_sha256.to_ascii_lowercase())
            .cloned()
            .ok_or_else(|| {
                EngineError::Operation("NInfer help observation was not cached".to_owned())
            })?;
        let mut definitions = settings::definitions();
        norted_engine::apply_artifact_stop_token_preview(&mut definitions, model);
        settings::apply_runtime_bounds(&mut definitions);
        let capabilities = ninfer_runtime_capabilities_for_installed(runtime);
        settings::apply_model_capabilities(
            &mut definitions,
            model,
            settings,
            capabilities.dflash_vision,
        );
        if capabilities.trustworthy_identity {
            settings::apply_reviewed_runtime_defaults(&mut definitions, settings);
        }
        if capabilities.model_sampler_defaults {
            settings::apply_model_sampler_defaults(&mut definitions, model, settings);
        }
        apply_ninfer_runtime_contract(&mut definitions, &help, capabilities);
        apply_ninfer_stop_token_contract(&mut definitions, runtime, settings);
        Ok(norted_core::SettingsSchema {
            engine_id: ENGINE_ID.to_owned(),
            runtime_id: Some(runtime.manifest.runtime_id.clone()),
            definitions,
        })
    }

    async fn probe(&self) -> Result<EngineProbe, EngineError> {
        Ok(self.probe_uncached().await)
    }

    async fn probe_runtime(
        &self,
        runtime: &InstalledRuntime,
    ) -> Result<RuntimeProbeObservation, EngineError> {
        if !self.enabled {
            return Err(EngineError::InvalidConfiguration(
                "NInfer adapter is disabled in configuration".to_owned(),
            ));
        }
        if let Some(error) = &self.configuration_error {
            return Err(EngineError::InvalidConfiguration(error.clone()));
        }
        runtime
            .manifest
            .identity
            .validate()
            .map_err(|error| EngineError::InvalidConfiguration(error.to_string()))?;
        if runtime.manifest.runtime_id != RuntimeId::from_identity(&runtime.manifest.identity) {
            return Err(EngineError::InvalidConfiguration(
                "runtime ID does not match its structured identity".to_owned(),
            ));
        }
        if let CompatibilityDecision::Unsupported { reason } = self.runtime_compatibility(runtime) {
            return Err(EngineError::InvalidConfiguration(reason));
        }
        let configured_path = runtime.entrypoint_path();
        if runtime.manifest.acquisition_method != RuntimeAcquisitionMethod::ExternalBinary {
            let root = tokio::fs::canonicalize(&runtime.installation_root)
                .await
                .map_err(|error| {
                    EngineError::InvalidConfiguration(format!(
                        "runtime installation root could not be resolved: {error}"
                    ))
                })?;
            let entrypoint = tokio::fs::canonicalize(&configured_path)
                .await
                .map_err(|error| {
                    EngineError::InvalidConfiguration(format!(
                        "runtime entrypoint could not be resolved: {error}"
                    ))
                })?;
            if !entrypoint.starts_with(&root) {
                return Err(EngineError::InvalidConfiguration(
                    "managed NInfer entrypoint escapes its installation root".to_owned(),
                ));
            }
        }
        let (_, _, observation) = self
            .inspect_binary(&configured_path, Some(&runtime.manifest.entrypoint_sha256))
            .await?;
        Ok(observation)
    }

    fn source_native_identities(
        &self,
        source_root: &Path,
    ) -> Result<Vec<ArtifactNativeIdentity>, EngineError> {
        Ok(inspect_source_native_identities(source_root))
    }

    async fn build_launch_spec(
        &self,
        mut request: LaunchRequest,
    ) -> Result<LaunchSpec, EngineError> {
        if !request.backend_address.ip().is_loopback() {
            return Err(EngineError::InvalidConfiguration(
                "NInfer backend address must be loopback".to_owned(),
            ));
        }
        let runtime_capabilities = ninfer_runtime_capabilities_for_installed(&request.runtime);
        if !runtime_capabilities.startup_proof {
            return Err(EngineError::InvalidConfiguration(
                "this NInfer source runtime's request-log/startup-proof capability contract is outdated or unreviewed; install or select the current reviewed runtime"
                    .to_owned(),
            ));
        }
        validate_ninfer_settings_prelaunch(&request.settings, runtime_capabilities)
            .map_err(EngineError::InvalidConfiguration)?;
        request
            .settings_schema
            .validate(&request.settings)
            .map_err(|error| EngineError::InvalidConfiguration(error.to_string()))?;
        settings::validate_model_settings(
            &request.settings,
            &request.model.primary,
            runtime_capabilities.dflash_vision,
        )
        .map_err(EngineError::InvalidConfiguration)?;
        let bound_context_cost = bind_ninfer_context_cost_presets(&mut request.settings).await?;
        let structured = settings::translate(
            &request.settings,
            &request.model.primary,
            &self.native_arguments,
            runtime_capabilities.dflash_vision,
        )?;
        let settings_requirements = (!request.settings.is_empty())
            .then(|| ninfer_startup_requirements(&request.settings))
            .transpose()?;
        let model_path =
            canonical_regular_file(&request.model.primary.path, "NInfer model").await?;
        if model_path != request.model.primary.path {
            return Err(EngineError::InvalidConfiguration(
                "prepared NInfer model path changed before launch".to_owned(),
            ));
        }
        let expected_identity = request
            .model
            .primary
            .native_identity
            .as_ref()
            .and_then(|identity| match identity {
                ArtifactNativeIdentity::Ninfer(identity) => Some(identity.clone()),
                ArtifactNativeIdentity::Gguf(_) => None,
            })
            .ok_or_else(|| {
                EngineError::InvalidConfiguration(
                    "prepared NInfer model has no native identity".to_owned(),
                )
            })?;
        let observed_identity = inspect_ninfer_container(&model_path)
            .map_err(|error| {
                EngineError::InvalidConfiguration(format!(
                    "could not revalidate NInfer artifact before launch: {error}"
                ))
            })?
            .identity;
        if observed_identity != expected_identity {
            return Err(EngineError::InvalidConfiguration(
                "NInfer artifact native identity changed after prepared-input validation"
                    .to_owned(),
            ));
        }
        let observation = self.probe_runtime(&request.runtime).await?;
        let binding = request.accelerator_binding.clone().ok_or_else(|| {
            EngineError::InvalidConfiguration(
                "NInfer launch has no exact NVIDIA GPU selected by compatibility evaluation"
                    .to_owned(),
            )
        })?;
        let [accelerator] = binding.devices.as_slice() else {
            return Err(EngineError::InvalidConfiguration(
                "NInfer requires an accelerator binding containing exactly one NVIDIA GPU"
                    .to_owned(),
            ));
        };
        let environment = isolated_cuda_environment(&self.environment, accelerator, "NInfer")
            .map_err(EngineError::InvalidConfiguration)?;
        let request_log_path = create_private_request_log()?;
        let endpoint = http_endpoint(request.backend_address);
        let public_model_id = request.settings.model_profile_id.clone().ok_or_else(|| {
            EngineError::InvalidConfiguration(
                "resolved settings do not identify the loaded Model Profile".to_owned(),
            )
        })?;
        let mut arguments = managed_launch_arguments(
            &model_path,
            request.backend_address,
            &public_model_id,
            &request_log_path,
        );
        arguments.extend(structured);
        arguments.extend(self.native_arguments.iter().map(OsString::from));

        let manifest = &request.runtime.manifest;
        let binary_path = request.runtime.entrypoint_path();
        let installation = EngineInstallation {
            engine: EngineRevision {
                engine_id: ENGINE_ID.to_owned(),
                version: observation.observed_version.clone(),
                revision: observation.observed_revision.clone(),
            },
            source_repository: manifest.identity.package.repository.clone(),
            acquisition_method: match manifest.acquisition_method {
                RuntimeAcquisitionMethod::OfficialReleaseAsset
                | RuntimeAcquisitionMethod::PreseededOfficialPack => {
                    AcquisitionMethod::OfficialBinary
                }
                RuntimeAcquisitionMethod::SourceBuild => AcquisitionMethod::SourceBuild,
                RuntimeAcquisitionMethod::ExternalBinary => AcquisitionMethod::ExternalBinary,
            },
            binary_path: binary_path.clone(),
            binary_sha256: Some(manifest.entrypoint_sha256.clone()),
            build: None,
            platform: manifest.identity.platform.clone(),
            architecture: manifest.identity.architecture.clone(),
            runtime_variant: Some(manifest.identity.variant.clone()),
            acquired_at_unix: manifest.installed_at_unix,
            observed_at_unix: observation.observed_at_unix,
        };

        if let Some(previous) = self.pending_startups.write().await.insert(
            endpoint.clone(),
            PendingStartup {
                request_log_path: request_log_path.clone(),
                native_identity: expected_identity.clone(),
                public_model_id: public_model_id.clone(),
                accelerator: accelerator.clone(),
                settings_requirements: settings_requirements.clone(),
                capabilities: runtime_capabilities,
            },
        ) {
            let _ = tokio::fs::remove_file(previous.request_log_path).await;
        }
        self.observed_defaults.write().await.remove(&endpoint);
        self.active_executions.write().await.remove(&endpoint);

        let mut normalized_settings = BTreeMap::from([
            (
                "container_version".to_owned(),
                json!(expected_identity.container_version),
            ),
            (
                "native_model_id".to_owned(),
                json!(expected_identity.model_id),
            ),
            (
                "native_weights_id".to_owned(),
                json!(expected_identity.weights_id),
            ),
            (
                "request_log_schema".to_owned(),
                json!(runtime_capabilities.request_log_schema),
            ),
        ]);
        if let Some(identity) = bound_context_cost {
            normalized_settings.insert(
                "bound_files.ninfer.context_cost_presets".to_owned(),
                identity,
            );
        }
        if let Some(requirements) = settings_requirements.as_ref() {
            normalized_settings.extend([
                (
                    "configured_ninfer.thinking".to_owned(),
                    json!(requirements.expected_thinking),
                ),
                (
                    "configured_ninfer.kv_dtype".to_owned(),
                    json!(requirements.kv_dtype),
                ),
                (
                    "configured_ninfer.speculative_backend".to_owned(),
                    json!(requirements.speculative_backend),
                ),
                (
                    "configured_ninfer.draft_tokens".to_owned(),
                    json!(requirements.speculative_draft_window),
                ),
                (
                    "configured_ninfer.lm_head_draft".to_owned(),
                    json!(requirements.proposal_head),
                ),
            ]);
            for (name, value) in [
                (
                    "configured_ninfer.temperature",
                    requirements.temperature.map(serde_json::Value::from),
                ),
                (
                    "configured_ninfer.top_p",
                    requirements.top_p.map(serde_json::Value::from),
                ),
                (
                    "configured_ninfer.top_k",
                    requirements.top_k.map(serde_json::Value::from),
                ),
                (
                    "configured_ninfer.min_p",
                    requirements.min_p.map(serde_json::Value::from),
                ),
                (
                    "configured_ninfer.context_length",
                    requirements
                        .minimum_context_tokens
                        .map(serde_json::Value::from),
                ),
            ] {
                if let Some(value) = value {
                    normalized_settings.insert(name.to_owned(), value);
                }
            }
        }

        Ok(LaunchSpec {
            executable: binary_path.clone(),
            arguments,
            environment,
            environment_remove: managed_environment_removals(),
            inherits_parent_environment: true,
            working_directory: binary_path.parent().map(PathBuf::from),
            temporary_files: vec![request_log_path],
            endpoint: Some(endpoint),
            normalized_settings,
            settings: request.settings,
            native_arguments: self.native_arguments.clone(),
            installation,
            runtime: request.runtime,
            model: request.model,
            accelerator_binding: Some(binding),
        })
    }

    fn prepare_launch_progress(&self, spec: &LaunchSpec) -> Option<BackendLoadProgress> {
        spec.model.primary.norted_package.as_ref().map(|_| {
            BackendLoadProgress::with_message(
                BackendLoadPhase::PreparingLaunch,
                "Revalidating package before launch",
            )
        })
    }

    async fn prepare_launch_attempt(&self, spec: &LaunchSpec) -> Result<(), EngineError> {
        revalidate_norted_package_before_launch(&spec.model).await
    }

    async fn prepare_launch_attempt_with_progress(
        &self,
        spec: &LaunchSpec,
        progress: LoadProgressReporter,
    ) -> Result<(), EngineError> {
        revalidate_norted_package_before_launch_with_progress(&spec.model, &progress).await
    }

    async fn health(&self, process: &ProcessDescriptor) -> Result<bool, EngineError> {
        let endpoint = process.endpoint.as_deref().ok_or_else(|| {
            EngineError::Operation("NInfer process has no backend endpoint".to_owned())
        })?;
        let response = self
            .client
            .get(format!("{endpoint}/health"))
            .timeout(HEALTH_TIMEOUT)
            .send()
            .await
            .map_err(|error| EngineError::BackendUnavailable(error.to_string()))?;
        if response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            return Ok(false);
        }
        if !response.status().is_success() {
            return Err(EngineError::BackendUnavailable(format!(
                "NInfer health endpoint returned HTTP {}",
                response.status()
            )));
        }
        let health = response.json::<HealthResponse>().await.map_err(|error| {
            EngineError::Operation(format!("invalid NInfer health response: {error}"))
        })?;
        if health.status != "ok" {
            return Ok(false);
        }
        if self.observed_defaults.read().await.contains_key(endpoint) {
            return Ok(true);
        }
        let pending = self
            .pending_startups
            .read()
            .await
            .get(endpoint)
            .cloned()
            .ok_or_else(|| {
                EngineError::InvalidConfiguration(
                    "NInfer startup provenance is unavailable for this process".to_owned(),
                )
            })?;
        let result = match read_and_validate_startup_log(&pending).await {
            Ok(Some(defaults)) => defaults,
            Ok(None) => return Ok(false),
            Err(error) => {
                self.pending_startups.write().await.remove(endpoint);
                let _ = tokio::fs::remove_file(&pending.request_log_path).await;
                // A rejected startup proof cannot recover by polling: its
                // pending state has been consumed. Preserve the cause and let
                // the manager stop this owned process immediately.
                return Err(EngineError::InvalidConfiguration(format!(
                    "NInfer startup validation failed: {error}"
                )));
            }
        };
        self.pending_startups.write().await.remove(endpoint);
        if let Err(error) = tokio::fs::remove_file(&pending.request_log_path).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(EngineError::InvalidConfiguration(format!(
                "could not unlink the private NInfer startup log before serving requests: {error}"
            )));
        }
        self.observed_defaults
            .write()
            .await
            .insert(endpoint.to_owned(), result);
        self.active_executions.write().await.insert(
            endpoint.to_owned(),
            NinferExecution {
                capabilities: pending.capabilities,
                vision: pending
                    .settings_requirements
                    .as_ref()
                    .and_then(|requirements| requirements.vision)
                    .unwrap_or(false),
                greedy: pending
                    .settings_requirements
                    .as_ref()
                    .and_then(|requirements| requirements.greedy)
                    .unwrap_or(false),
            },
        );
        Ok(true)
    }

    async fn clear_launch_state(&self, endpoint: Option<&str>) {
        if let Some(endpoint) = endpoint {
            if let Some(pending) = self.pending_startups.write().await.remove(endpoint) {
                let _ = tokio::fs::remove_file(pending.request_log_path).await;
            }
            self.observed_defaults.write().await.remove(endpoint);
            self.active_executions.write().await.remove(endpoint);
        }
    }

    fn startup_progress(&self, stderr_tail: &[String]) -> Option<BackendLoadProgress> {
        parse_ninfer_startup_progress(stderr_tail)
    }

    async fn startup_observation(
        &self,
        process: &ProcessDescriptor,
        _stderr_tail: &[String],
    ) -> Result<norted_engine::StartupObservation, EngineError> {
        let endpoint = process.endpoint.as_deref().ok_or_else(|| {
            EngineError::Operation("NInfer process has no backend endpoint".to_owned())
        })?;
        let observed = self
            .observed_defaults
            .read()
            .await
            .get(endpoint)
            .cloned()
            .ok_or_else(|| {
                EngineError::BackendUnavailable(
                    "NInfer startup settings have not been observed yet".to_owned(),
                )
            })?;
        Ok(norted_engine::StartupObservation::Ready(BTreeMap::from([
            (
                "kv_cache_format".to_owned(),
                json!(observed.kv_cache_format),
            ),
            (
                "resolved_settings".to_owned(),
                serde_json::to_value(observed.resolved_settings).map_err(|error| {
                    EngineError::Operation(format!(
                        "could not serialize NInfer resolved settings: {error}"
                    ))
                })?,
            ),
        ])))
    }

    async fn effective_generation_settings(
        &self,
        process: &ProcessDescriptor,
    ) -> Result<EffectiveGenerationSettings, EngineError> {
        let endpoint = process.endpoint.as_deref().ok_or_else(|| {
            EngineError::Operation("NInfer process has no backend endpoint".to_owned())
        })?;
        self.observed_defaults
            .read()
            .await
            .get(endpoint)
            .map(|observed| observed.generation_settings.clone())
            .ok_or_else(|| {
                EngineError::Operation(
                    "NInfer effective sampler defaults were not validated at startup".to_owned(),
                )
            })
    }

    fn validate_generation_settings(
        &self,
        settings: &GenerationSettingsPatch,
        _backend_defaults: &EffectiveGenerationSettings,
    ) -> Result<(), EngineError> {
        settings.validate_stop_token_ids()?;

        if settings.repeat_penalty.is_some_and(|value| value != 1.0) {
            return Err(EngineError::InvalidGenerationSettings(
                "NInfer supports only the neutral repetition penalty 1.0".to_owned(),
            ));
        }
        if settings
            .temperature
            .is_some_and(|value| !value.is_finite() || !(0.0..=2.0).contains(&value))
        {
            return Err(EngineError::InvalidGenerationSettings(
                "NInfer temperature must be finite and in 0..=2".to_owned(),
            ));
        }
        if settings
            .top_p
            .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            return Err(EngineError::InvalidGenerationSettings(
                "NInfer top_p must be finite and in 0..=1".to_owned(),
            ));
        }
        if settings.top_k.is_some_and(|value| value > 20) {
            return Err(EngineError::InvalidGenerationSettings(
                "NInfer top_k must be in 0..=20".to_owned(),
            ));
        }
        if settings
            .min_p
            .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
        {
            return Err(EngineError::InvalidGenerationSettings(
                "NInfer min_p must be finite and in 0..=1".to_owned(),
            ));
        }
        for (name, value) in [
            ("ninfer.presence_penalty", settings.presence_penalty),
            ("ninfer.frequency_penalty", settings.frequency_penalty),
        ] {
            if value.is_some_and(|value| !value.is_finite() || !(-2.0..=2.0).contains(&value)) {
                return Err(EngineError::InvalidGenerationSettings(format!(
                    "NInfer {name} must be finite and in -2..=2"
                )));
            }
        }
        if settings.seed.is_some_and(|seed| seed > i64::MAX as u64) {
            return Err(EngineError::InvalidGenerationSettings(
                "NInfer seed must fit its signed 64-bit request contract".to_owned(),
            ));
        }
        if let Some(stop) = settings.stop.as_ref()
            && (stop.len() > 4 || stop.iter().any(String::is_empty))
        {
            return Err(EngineError::InvalidGenerationSettings(
                "NInfer accepts at most four non-empty stop strings".to_owned(),
            ));
        }
        if settings.reasoning_budget.is_some() {
            return Err(EngineError::InvalidGenerationSettings(
                "NInfer has no Chat Completions per-request thinking budget; configure the launch default instead"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_inference_request(
        &self,
        request: &InferenceRequest,
        backend_defaults: &EffectiveGenerationSettings,
        _settings_schema: &norted_core::SettingsSchema,
    ) -> Result<(), EngineError> {
        self.validate_generation_settings(&request.generation_settings, backend_defaults)?;
        if matches!(
            request.output_format.as_ref(),
            Some(OutputFormat::JsonObject | OutputFormat::JsonSchema { .. })
        ) {
            return Err(EngineError::InvalidGenerationSettings(
                "NInfer does not support structured output".to_owned(),
            ));
        }
        Ok(())
    }

    async fn infer(
        &self,
        endpoint: &str,
        request: InferenceRequest,
    ) -> Result<InferenceOutput, EngineError> {
        let execution = self
            .active_executions
            .read()
            .await
            .get(endpoint)
            .copied()
            .ok_or_else(|| {
                EngineError::Operation(
                    "NInfer request capability proof is unavailable for this process".to_owned(),
                )
            })?;
        let body = protocol::backend_request(
            &request,
            false,
            protocol::RequestAdmission {
                protocol_semantics: execution.capabilities.request_protocol_semantics,
                sampler_semantics: execution.capabilities.request_sampler_semantics,
                thinking_semantics: execution.capabilities.thinking_request_semantics,
                tool_calling: execution.capabilities.tool_calling,
                media_semantics: execution.capabilities.media_request_semantics,
                vision: execution.vision,
                greedy: execution.greedy,
            },
        )?;
        let response = self
            .client
            .post(format!("{endpoint}/v1/chat/completions"))
            .timeout(INFERENCE_TIMEOUT)
            .json(&body)
            .send()
            .await
            .map_err(protocol::map_transport_error)?;
        protocol::parse_completion_response(response).await
    }

    async fn infer_stream(
        &self,
        endpoint: &str,
        request: InferenceRequest,
        _activity: InferenceActivityReporter,
    ) -> Result<InferenceStream, EngineError> {
        let execution = self
            .active_executions
            .read()
            .await
            .get(endpoint)
            .copied()
            .ok_or_else(|| {
                EngineError::Operation(
                    "NInfer request capability proof is unavailable for this process".to_owned(),
                )
            })?;
        let body = protocol::backend_request(
            &request,
            true,
            protocol::RequestAdmission {
                protocol_semantics: execution.capabilities.request_protocol_semantics,
                sampler_semantics: execution.capabilities.request_sampler_semantics,
                thinking_semantics: execution.capabilities.thinking_request_semantics,
                tool_calling: execution.capabilities.tool_calling,
                media_semantics: execution.capabilities.media_request_semantics,
                vision: execution.vision,
                greedy: execution.greedy,
            },
        )?;
        let response = self
            .client
            .post(format!("{endpoint}/v1/chat/completions"))
            .timeout(INFERENCE_TIMEOUT)
            .json(&body)
            .send()
            .await
            .map_err(protocol::map_transport_error)?;
        protocol::parse_stream_response(response).await
    }
}

#[derive(Debug)]
struct DeviceEvaluation {
    compatibility: RuntimeCompatibility,
    accelerator: Option<AcceleratorDevice>,
}

fn ninfer_device_evaluation(
    platform: &str,
    architecture: &str,
    requirements: &RuntimeRequirements,
    host: &HostCapabilities,
    external: bool,
) -> DeviceEvaluation {
    let devices = match visible_nvidia_devices(host, "NInfer") {
        Ok(devices) => devices,
        Err(compatibility) => {
            return DeviceEvaluation {
                compatibility,
                accelerator: None,
            };
        }
    };
    let mut requirements = requirements.clone();
    requirements.requires_nvidia_gpu = true;
    if external {
        requirements.unverified_requirements.push(
            "external ninfer-serve GPU targets and exact native artifact registry are unverified"
                .to_owned(),
        );
    }
    let mut evaluated = devices
        .into_iter()
        .map(|device| {
            let device_host = HostCapabilities {
                platform: host.platform.clone(),
                architecture: host.architecture.clone(),
                accelerators: vec![device.clone()],
                nvidia_gpu_absence_confirmed: false,
                cuda_visible_devices: None,
                observations: Vec::new(),
            };
            (
                device.clone(),
                compatibility_for(platform, architecture, "cuda", &requirements, &device_host),
            )
        })
        .collect::<Vec<_>>();
    evaluated.sort_by(|left, right| {
        left.1
            .preference_rank()
            .cmp(&right.1.preference_rank())
            .then_with(|| {
                right
                    .0
                    .vram_bytes
                    .unwrap_or(0)
                    .cmp(&left.0.vram_bytes.unwrap_or(0))
            })
            .then_with(|| left.0.stable_id.cmp(&right.0.stable_id))
    });
    match evaluated.into_iter().next() {
        Some((accelerator, compatibility)) => DeviceEvaluation {
            compatibility,
            accelerator: Some(accelerator),
        },
        None => DeviceEvaluation {
            compatibility: RuntimeCompatibility::NeedsAttention(
                "NInfer requires a stable NVIDIA GPU UUID, but none was observed".to_owned(),
            ),
            accelerator: None,
        },
    }
}

fn native_compatibility(
    supported: &[ArtifactNativeIdentity],
    model: &ArtifactNativeIdentity,
) -> RuntimeCompatibility {
    if supported.is_empty() {
        RuntimeCompatibility::NeedsAttention(
            "the exact ninfer-serve target registry could not be enumerated independently"
                .to_owned(),
        )
    } else if supported.contains(model) {
        RuntimeCompatibility::Recommended
    } else {
        match model {
            ArtifactNativeIdentity::Ninfer(identity) => {
                RuntimeCompatibility::Incompatible(format!(
                    "runtime does not declare support for NInfer target `{}/{}`",
                    identity.model_id, identity.weights_id
                ))
            }
            ArtifactNativeIdentity::Gguf(_) => RuntimeCompatibility::Incompatible(
                "NInfer runtime received a GGUF native identity".to_owned(),
            ),
        }
    }
}

fn combine_compatibility(
    left: RuntimeCompatibility,
    right: RuntimeCompatibility,
) -> RuntimeCompatibility {
    match (left, right) {
        (RuntimeCompatibility::Incompatible(reason), _)
        | (_, RuntimeCompatibility::Incompatible(reason)) => {
            RuntimeCompatibility::Incompatible(reason)
        }
        (RuntimeCompatibility::NeedsAttention(reason), _)
        | (_, RuntimeCompatibility::NeedsAttention(reason)) => {
            RuntimeCompatibility::NeedsAttention(reason)
        }
        (RuntimeCompatibility::Recommended, RuntimeCompatibility::Recommended) => {
            RuntimeCompatibility::Recommended
        }
        _ => RuntimeCompatibility::Compatible,
    }
}

#[derive(Deserialize)]
struct HealthResponse {
    status: String,
}

#[derive(Deserialize)]
struct StartupRecord {
    artifact_type: String,
    schema_version: u32,
    event: String,
    server: StartupServer,
    artifact: StartupArtifact,
    engine: StartupEngine,
    sampling_defaults: StartupSamplingDefaults,
    environment: StartupEnvironment,
}

#[derive(Deserialize)]
struct StartupServer {
    public_model_id: String,
    default_thinking: bool,
    default_preserve_thinking: bool,
    max_request_bytes: u64,
    media_cache_bytes: u64,
    media_live_bytes: u64,
    media_preprocess_threads: u64,
    default_output_tokens: u64,
    default_thinking_budget: Option<u64>,
}

#[derive(Deserialize)]
struct StartupArtifact {
    target: String,
    weights_id: String,
}

#[derive(Deserialize)]
struct StartupEngine {
    max_context: u64,
    kv_capacity_mode: String,
    kv_capacity: u64,
    kv_cache: String,
    max_concurrency: u64,
    vision: bool,
    cuda_graph: bool,
    prefix_reuse: bool,
    speculative_backend: String,
    speculative_draft_window: u64,
    proposal_head: String,
    max_pending_requests: u64,
    pending_timeout_ms: u64,
    prefill_chunk: u64,
    log_stats_interval_ms: u64,
    context_cost: StartupContextCost,
    context_cache: StartupContextCache,
}

#[derive(Deserialize)]
struct StartupContextCache {
    enabled: bool,
    device_state_slots: u64,
    host_state_slots: u64,
    host_kv_capacity_bytes: u64,
    max_private_continuations: u64,
    max_shared_prefixes: u64,
    max_long_anchors_per_continuation: u64,
}

#[derive(Clone)]
struct ObservedNinferStartup {
    kv_cache_format: String,
    generation_settings: EffectiveGenerationSettings,
    resolved_settings: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct StartupContextCost {
    model_id: String,
    weights_id: String,
}

#[derive(Deserialize)]
struct StartupSamplingDefaults {
    thinking: StartupPreset,
    non_thinking: StartupPreset,
    server_overrides: StartupOverrides,
    greedy: bool,
}

#[derive(Clone, Copy, Deserialize)]
struct StartupPreset {
    temperature: f64,
    top_p: f64,
    top_k: u64,
    min_p: f64,
    presence_penalty: f64,
    frequency_penalty: f64,
}

#[derive(Deserialize)]
struct StartupOverrides {
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<u64>,
    min_p: Option<f64>,
    #[serde(default)]
    presence_penalty: Option<f64>,
    #[serde(default)]
    frequency_penalty: Option<f64>,
    #[serde(default)]
    seed: Option<u64>,
}

#[derive(Deserialize)]
struct StartupEnvironment {
    gpu_name: String,
    gpu_uuid: String,
    compute_capability_major: u16,
    compute_capability_minor: u16,
}

/// Best-effort UX progress from ninfer-serve stderr. NInfer's authoritative
/// startup contract is the private JSONL request log validated in `health`,
/// not stderr; this parser only recognizes a few conservative, stable
/// prefixes and degrades to `None` for unknown output. It never invents a
/// fraction and is never an admission gate.
fn parse_ninfer_startup_progress(stderr_tail: &[String]) -> Option<BackendLoadProgress> {
    let mut progress = None;
    for line in stderr_tail {
        let line = line.trim();
        if line.contains("listening on")
            || line.contains("server started")
            || line.contains("ninfer-serve ready")
        {
            progress = Some(BackendLoadProgress::with_message(
                BackendLoadPhase::VerifyingStartup,
                "Starting the NInfer server",
            ));
        } else if line.contains("loading model")
            || line.contains("loading weights")
            || line.contains("loading artifact")
        {
            progress = Some(BackendLoadProgress::with_message(
                BackendLoadPhase::LoadingModel,
                "Loading model weights",
            ));
        } else if line.contains("allocating kv")
            || line.contains("allocating context")
            || line.contains("initializing kv cache")
        {
            progress = Some(BackendLoadProgress::with_message(
                BackendLoadPhase::AllocatingContext,
                "Allocating context and KV cache",
            ));
        }
    }
    progress
}

async fn read_and_validate_startup_log(
    pending: &PendingStartup,
) -> Result<Option<ObservedNinferStartup>, EngineError> {
    let file = tokio::fs::File::open(&pending.request_log_path)
        .await
        .map_err(|error| {
            EngineError::Operation(format!("could not open NInfer startup log: {error}"))
        })?;
    let mut bytes = Vec::new();
    file.take(STARTUP_LOG_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| {
            EngineError::Operation(format!("could not read NInfer startup log: {error}"))
        })?;
    if bytes.len() as u64 > STARTUP_LOG_LIMIT {
        return Err(EngineError::Operation(
            "NInfer startup log exceeded the local size limit".to_owned(),
        ));
    }
    let mut startup = None;
    for line in bytes.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        if line.len() > STARTUP_LOG_LINE_LIMIT {
            return Err(EngineError::Operation(
                "NInfer startup log record exceeded the local size limit".to_owned(),
            ));
        }
        let value: serde_json::Value = serde_json::from_slice(line).map_err(|error| {
            EngineError::Operation(format!("invalid NInfer startup JSONL record: {error}"))
        })?;
        if value.get("event").and_then(serde_json::Value::as_str) == Some("server_start") {
            if startup.is_some() {
                return Err(EngineError::Operation(
                    "NInfer startup log contained multiple server_start records".to_owned(),
                ));
            }
            startup = Some(
                serde_json::from_value::<StartupRecord>(value).map_err(|error| {
                    EngineError::Operation(format!("invalid NInfer server_start record: {error}"))
                })?,
            );
        }
    }
    let Some(startup) = startup else {
        return Ok(None);
    };
    let expected_schema = pending.capabilities.request_log_schema.ok_or_else(|| {
        EngineError::Operation("NInfer startup proof has no reviewed request-log schema".to_owned())
    })?;
    let schema_supported = startup.schema_version == expected_schema;
    if startup.artifact_type != "ninfer_serve_request_log"
        || !schema_supported
        || startup.event != "server_start"
    {
        return Err(EngineError::Operation(
            "NInfer startup record has an unsupported artifact type or schema".to_owned(),
        ));
    }
    if startup.server.public_model_id != pending.public_model_id.as_str() {
        return Err(EngineError::Operation(
            "NInfer startup public model ID differs from the Model Profile/public alias".to_owned(),
        ));
    }
    // In the reviewed NInfer registry, load.target is a dispatch key while
    // context_cost.model_id is the artifact's native model ID. They are not
    // aliases of the public Model Profile ID and must be checked separately.
    // Keep this mapping explicit: punctuation normalization could admit an
    // unreviewed target. Unknown native identities remain unsupported.
    let expected_target = match pending.native_identity.model_id.as_str() {
        "qwen3.6-27b" => "qwen3_6_27b",
        "qwen3.8-27b" => "qwen3_8_27b",
        "qwen3.6-35b-a3b" => "qwen3_6_35b_a3b",
        model => {
            return Err(EngineError::Operation(format!(
                "NInfer startup target mapping is unreviewed for native model `{model}`"
            )));
        }
    };
    if startup.artifact.target != expected_target {
        return Err(EngineError::Operation(format!(
            "NInfer startup target `{}` differs from expected target `{expected_target}` for native model `{}`",
            startup.artifact.target, pending.native_identity.model_id
        )));
    }
    if startup.artifact.weights_id != pending.native_identity.weights_id
        || startup.engine.context_cost.model_id != pending.native_identity.model_id
        || startup.engine.context_cost.weights_id != pending.native_identity.weights_id
    {
        return Err(EngineError::Operation(
            "NInfer startup native model or weights differ from the inspected artifact identity"
                .to_owned(),
        ));
    }
    let expected_uuid = pending.accelerator.stable_id.as_deref().ok_or_else(|| {
        EngineError::Operation("selected NInfer accelerator has no GPU UUID".to_owned())
    })?;
    if startup.environment.gpu_uuid != expected_uuid {
        return Err(EngineError::Operation(
            "NInfer reported a different GPU UUID than the isolated selected device".to_owned(),
        ));
    }
    if let Some(expected_name) = pending.accelerator.name.as_deref()
        && startup.environment.gpu_name != expected_name
    {
        return Err(EngineError::Operation(
            "NInfer reported a different GPU name than the selected device".to_owned(),
        ));
    }
    if let Some(expected) = pending.accelerator.compute_capability
        && (startup.environment.compute_capability_major != expected.major
            || startup.environment.compute_capability_minor != expected.minor)
    {
        return Err(EngineError::Operation(
            "NInfer reported a different compute capability than the selected device".to_owned(),
        ));
    }
    // CLI choices and request-log storage descriptions name the same enum
    // differently. Decode only the formats proved by the reviewed log schema.
    let kv_dtype = match startup.engine.kv_cache.as_str() {
        "bf16" => "bf16",
        "int8-group64" => "int8",
        "fp8-e4m3-row256" => "fp8",
        "nvfp4" => "nvfp4",
        "k8v4" => "k8v4",
        format => {
            return Err(EngineError::Operation(format!(
                "NInfer startup reported an unreviewed KV-cache format `{format}`"
            )));
        }
    };
    let preset = if startup.server.default_thinking {
        startup.sampling_defaults.thinking
    } else {
        startup.sampling_defaults.non_thinking
    };
    let defaults = EffectiveGenerationSettings {
        stop_token_ids: None,
        required_stop_token_ids: Vec::new(),
        temperature: if startup.sampling_defaults.greedy {
            0.0
        } else {
            startup
                .sampling_defaults
                .server_overrides
                .temperature
                .unwrap_or(preset.temperature)
        },
        top_p: startup
            .sampling_defaults
            .server_overrides
            .top_p
            .unwrap_or(preset.top_p),
    };
    let effective_top_k = startup
        .sampling_defaults
        .server_overrides
        .top_k
        .unwrap_or(preset.top_k);
    let effective_min_p = startup
        .sampling_defaults
        .server_overrides
        .min_p
        .unwrap_or(preset.min_p);
    let effective_presence_penalty = startup
        .sampling_defaults
        .server_overrides
        .presence_penalty
        .unwrap_or(preset.presence_penalty);
    let effective_frequency_penalty = startup
        .sampling_defaults
        .server_overrides
        .frequency_penalty
        .unwrap_or(preset.frequency_penalty);
    if !defaults.temperature.is_finite()
        || !(0.0..=2.0).contains(&defaults.temperature)
        || !defaults.top_p.is_finite()
        || !(0.0..=1.0).contains(&defaults.top_p)
    {
        return Err(EngineError::Operation(
            "NInfer startup sampler defaults are outside the supported API ranges".to_owned(),
        ));
    }
    if let Some(requirements) = pending.settings_requirements.as_ref() {
        if startup.sampling_defaults.greedy != requirements.greedy.unwrap_or(false) {
            return Err(EngineError::Operation(
                "NInfer startup greedy mode disagrees with the configured value".to_owned(),
            ));
        }
        if requirements
            .expected_thinking
            .is_some_and(|expected| startup.server.default_thinking != expected)
        {
            return Err(EngineError::Operation(format!(
                "NInfer startup default_thinking={} disagrees with the configured value {:?}",
                startup.server.default_thinking, requirements.expected_thinking
            )));
        }
        if requirements
            .preserve_thinking
            .is_some_and(|expected| startup.server.default_preserve_thinking != expected)
        {
            return Err(EngineError::Operation(
                "NInfer startup preserve-thinking behavior disagrees with the configured value"
                    .to_owned(),
            ));
        }
        if requirements
            .vision
            .is_some_and(|expected| startup.engine.vision != expected)
        {
            return Err(EngineError::Operation(
                "NInfer startup vision residency disagrees with the configured value".to_owned(),
            ));
        }
        if requirements
            .default_output_tokens
            .is_some_and(|expected| startup.server.default_output_tokens != expected)
            || requirements
                .default_thinking_budget
                .is_some_and(|expected| startup.server.default_thinking_budget != Some(expected))
        {
            return Err(EngineError::Operation(
                "NInfer startup output/thinking defaults disagree with configured values"
                    .to_owned(),
            ));
        }
        if requirements
            .max_pending_requests
            .is_some_and(|expected| startup.engine.max_pending_requests != expected)
            || requirements
                .pending_timeout_ms
                .is_some_and(|expected| startup.engine.pending_timeout_ms != expected)
            || requirements
                .log_stats_interval_ms
                .is_some_and(|expected| startup.engine.log_stats_interval_ms != expected)
        {
            return Err(EngineError::Operation(
                "NInfer startup queue/statistics controls disagree with configured values"
                    .to_owned(),
            ));
        }
        if requirements
            .max_request_bytes
            .is_some_and(|expected| startup.server.max_request_bytes != expected)
            || requirements
                .media_cache_bytes
                .is_some_and(|expected| startup.server.media_cache_bytes != expected)
            || requirements
                .media_live_bytes
                .is_some_and(|expected| startup.server.media_live_bytes != expected)
            || requirements
                .media_preprocess_threads
                .is_some_and(|expected| startup.server.media_preprocess_threads != expected)
        {
            return Err(EngineError::Operation(
                "NInfer startup request/media resource controls disagree with configured values"
                    .to_owned(),
            ));
        }
        if let Some(minimum_context) = requirements.minimum_context_tokens
            && startup.engine.max_context < minimum_context
        {
            return Err(EngineError::Operation(format!(
                "NInfer startup served only {} context tokens; the configured value requires at least {}",
                startup.engine.max_context, minimum_context
            )));
        }
        if requirements
            .max_concurrency
            .is_some_and(|expected| startup.engine.max_concurrency != expected)
            || requirements
                .prefill_chunk
                .is_some_and(|expected| startup.engine.prefill_chunk != expected)
        {
            return Err(EngineError::Operation(
                "NInfer startup concurrency/prefill controls disagree with configured values"
                    .to_owned(),
            ));
        }
        if requirements
            .kv_capacity_mode
            .as_ref()
            .is_some_and(|expected| startup.engine.kv_capacity_mode != *expected)
            || requirements
                .kv_capacity
                .is_some_and(|expected| startup.engine.kv_capacity != expected)
        {
            return Err(EngineError::Operation(
                "NInfer startup KV-capacity policy disagrees with the configured value".to_owned(),
            ));
        }
        if !matches!(
            startup.engine.kv_capacity_mode.as_str(),
            "auto" | "explicit"
        ) || startup.engine.kv_capacity < startup.engine.max_context
            || requirements
                .minimum_context_tokens
                .is_some_and(|minimum| startup.engine.kv_capacity < minimum)
        {
            return Err(EngineError::Operation(format!(
                "NInfer startup KV capacity {} ({}) does not prove the {:?}-token configured contract",
                startup.engine.kv_capacity,
                startup.engine.kv_capacity_mode,
                requirements.minimum_context_tokens
            )));
        }
        if requirements
            .kv_dtype
            .as_ref()
            .is_some_and(|expected| kv_dtype != expected)
            || requirements
                .cuda_graph
                .is_some_and(|expected| startup.engine.cuda_graph != expected)
            || requirements
                .prefix_reuse
                .is_some_and(|expected| startup.engine.prefix_reuse != expected)
        {
            return Err(EngineError::Operation(
                "NInfer startup did not prove the configured KV/CUDA-graph/prefix-reuse behavior"
                    .to_owned(),
            ));
        }
        let cache = &startup.engine.context_cache;
        if requirements
            .prefix_reuse
            .is_some_and(|expected| cache.enabled != expected)
            || requirements
                .device_state_slots
                .is_some_and(|expected| cache.device_state_slots != expected)
            || requirements
                .host_state_slots
                .is_some_and(|expected| cache.host_state_slots != expected)
            || requirements
                .host_kv_bytes
                .is_some_and(|expected| cache.host_kv_capacity_bytes != expected)
            || requirements
                .max_private_continuations
                .is_some_and(|expected| cache.max_private_continuations != expected)
            || requirements
                .max_shared_prefixes
                .is_some_and(|expected| cache.max_shared_prefixes != expected)
            || requirements
                .max_long_anchors_per_continuation
                .is_some_and(|expected| cache.max_long_anchors_per_continuation != expected)
        {
            return Err(EngineError::Operation(
                "NInfer startup context-cache controls disagree with configured values".to_owned(),
            ));
        }
        if requirements
            .speculative_backend
            .as_ref()
            .is_some_and(|expected| startup.engine.speculative_backend != *expected)
            || requirements
                .speculative_draft_window
                .is_some_and(|expected| startup.engine.speculative_draft_window != expected)
            || requirements
                .proposal_head
                .as_ref()
                .is_some_and(|expected| startup.engine.proposal_head != *expected)
        {
            return Err(EngineError::Operation(
                "NInfer startup did not prove the configured speculative behavior".to_owned(),
            ));
        }
        if requirements
            .temperature
            .is_some_and(|expected| !matches_runtime_float(defaults.temperature, expected))
            || requirements
                .top_p
                .is_some_and(|expected| !matches_runtime_float(defaults.top_p, expected))
            || requirements
                .top_k
                .is_some_and(|expected| effective_top_k != expected)
            || requirements
                .min_p
                .is_some_and(|expected| !matches_runtime_float(effective_min_p, expected))
        {
            return Err(EngineError::Operation(
                "NInfer startup did not resolve the configured sampler defaults".to_owned(),
            ));
        }
        if requirements.presence_penalty.is_some_and(|expected| {
            startup
                .sampling_defaults
                .server_overrides
                .presence_penalty
                .is_none_or(|observed| !matches_runtime_float(observed, expected))
        }) || requirements.frequency_penalty.is_some_and(|expected| {
            startup
                .sampling_defaults
                .server_overrides
                .frequency_penalty
                .is_none_or(|observed| !matches_runtime_float(observed, expected))
        }) || requirements.seed.is_some_and(|expected| {
            startup.sampling_defaults.server_overrides.seed != Some(expected)
        }) {
            return Err(EngineError::Operation(
                "NInfer startup penalty/seed defaults disagree with configured values".to_owned(),
            ));
        }
    }
    let mut resolved_settings = BTreeMap::from([
        (
            "ninfer.context_length".to_owned(),
            json!(startup.engine.max_context),
        ),
        (
            "ninfer.parallel_requests".to_owned(),
            json!(startup.engine.max_concurrency),
        ),
        ("ninfer.kv_dtype".to_owned(), json!(kv_dtype)),
        (
            "ninfer.kv_capacity".to_owned(),
            json!(startup.engine.kv_capacity),
        ),
        (
            "ninfer.prefill_chunk".to_owned(),
            json!(startup.engine.prefill_chunk),
        ),
        (
            "ninfer.cuda_graph".to_owned(),
            json!(startup.engine.cuda_graph),
        ),
        (
            "ninfer.prefix_reuse".to_owned(),
            json!(startup.engine.prefix_reuse),
        ),
        ("ninfer.vision".to_owned(), json!(startup.engine.vision)),
        (
            "ninfer.device_state_slots".to_owned(),
            json!(startup.engine.context_cache.device_state_slots),
        ),
        (
            "ninfer.host_state_slots".to_owned(),
            json!(startup.engine.context_cache.host_state_slots),
        ),
        (
            "ninfer.host_kv_mib".to_owned(),
            json!(startup.engine.context_cache.host_kv_capacity_bytes / (1_u64 << 20)),
        ),
        (
            "ninfer.max_private_continuations".to_owned(),
            json!(startup.engine.context_cache.max_private_continuations),
        ),
        (
            "ninfer.max_shared_prefixes".to_owned(),
            json!(startup.engine.context_cache.max_shared_prefixes),
        ),
        (
            "ninfer.max_long_anchors_per_continuation".to_owned(),
            json!(
                startup
                    .engine
                    .context_cache
                    .max_long_anchors_per_continuation
            ),
        ),
        (
            "ninfer.max_pending_requests".to_owned(),
            json!(startup.engine.max_pending_requests),
        ),
        (
            "ninfer.pending_timeout_ms".to_owned(),
            json!(startup.engine.pending_timeout_ms),
        ),
        (
            "ninfer.log_stats_interval_ms".to_owned(),
            json!(startup.engine.log_stats_interval_ms),
        ),
        (
            "ninfer.max_output_tokens".to_owned(),
            json!(startup.server.default_output_tokens),
        ),
        (
            "ninfer.reasoning_budget".to_owned(),
            startup
                .server
                .default_thinking_budget
                .map_or_else(|| json!("unlimited"), serde_json::Value::from),
        ),
        ("ninfer.temperature".to_owned(), json!(defaults.temperature)),
        ("ninfer.top_p".to_owned(), json!(defaults.top_p)),
        ("ninfer.top_k".to_owned(), json!(effective_top_k)),
        ("ninfer.min_p".to_owned(), json!(effective_min_p)),
        (
            "ninfer.presence_penalty".to_owned(),
            json!(effective_presence_penalty),
        ),
        (
            "ninfer.frequency_penalty".to_owned(),
            json!(effective_frequency_penalty),
        ),
        (
            "ninfer.seed".to_owned(),
            startup
                .sampling_defaults
                .server_overrides
                .seed
                .map_or_else(|| json!("random"), serde_json::Value::from),
        ),
    ]);
    if pending
        .settings_requirements
        .as_ref()
        .is_none_or(|requirements| requirements.reconcile_reasoning_from_startup)
    {
        resolved_settings.insert(
            "ninfer.reasoning".to_owned(),
            json!(if startup.server.default_thinking {
                "on"
            } else {
                "off"
            }),
        );
    }
    Ok(Some(ObservedNinferStartup {
        kv_cache_format: startup.engine.kv_cache,
        generation_settings: defaults,
        resolved_settings,
    }))
}

fn matches_runtime_float(observed: f64, requested: f64) -> bool {
    // NInfer parses process sampler controls as float (f32) and its JSON log
    // widens those exact values to double. Parse the same decimal argument
    // emitted by settings::push_value directly as f32 to avoid double rounding
    // at a midpoint, then require equality with the reported runtime value.
    observed.is_finite()
        && requested.is_finite()
        && requested
            .to_string()
            .parse::<f32>()
            .is_ok_and(|expected| observed == f64::from(expected))
}

fn create_private_request_log() -> Result<PathBuf, EngineError> {
    let temporary = tempfile::Builder::new()
        .prefix("norted-ninfer-")
        .suffix(".jsonl")
        .tempfile()
        .map_err(|error| {
            EngineError::Operation(format!(
                "could not create private NInfer startup log: {error}"
            ))
        })?;
    let (_file, path) = temporary.keep().map_err(|error| {
        EngineError::Operation(format!(
            "could not retain private NInfer startup log: {error}"
        ))
    })?;
    Ok(path)
}

fn managed_launch_arguments(
    model_path: &Path,
    backend_address: SocketAddr,
    public_model_id: &ModelProfileId,
    request_log_path: &Path,
) -> Vec<OsString> {
    vec![
        model_path.as_os_str().to_owned(),
        OsString::from("--host"),
        OsString::from(backend_address.ip().to_string()),
        OsString::from("--port"),
        OsString::from(backend_address.port().to_string()),
        OsString::from("--model-id"),
        OsString::from(public_model_id.as_str()),
        OsString::from("--device"),
        OsString::from("0"),
        OsString::from("--request-log-jsonl"),
        request_log_path.as_os_str().to_owned(),
    ]
}

fn inspect_source_native_identities(source_root: &Path) -> Vec<ArtifactNativeIdentity> {
    let targets = source_root.join("src").join("targets");
    let Ok(entries) = std::fs::read_dir(&targets) else {
        return Vec::new();
    };
    let mut identities = BTreeSet::new();
    let mut package_count = 0_usize;
    for entry in entries {
        let Ok(entry) = entry else {
            return Vec::new();
        };
        let target = entry.path();
        if !target.is_dir() {
            continue;
        }
        let package = target.join("impl").join("package.cpp");
        if !package.is_file() {
            continue;
        }
        package_count += 1;
        let Some(symbols) = parse_target_symbols(&target) else {
            return Vec::new();
        };
        let Ok(package_source) = std::fs::read_to_string(&package) else {
            return Vec::new();
        };
        let mut conditions = 0_usize;
        for line in package_source.lines() {
            if !line.contains("identity.model_id ==") {
                continue;
            }
            conditions += 1;
            let Some((symbol, weights_id)) = parse_identity_condition(line) else {
                return Vec::new();
            };
            let Some(model_id) = symbols.get(symbol) else {
                return Vec::new();
            };
            identities.insert(ArtifactNativeIdentity::Ninfer(NinferArtifactIdentity {
                container_version: 2,
                model_id: model_id.clone(),
                weights_id: weights_id.to_owned(),
            }));
        }
        if conditions == 0 {
            return Vec::new();
        }
    }
    if package_count == 0 {
        Vec::new()
    } else {
        identities.into_iter().collect()
    }
}

fn parse_target_symbols(target: &Path) -> Option<BTreeMap<String, String>> {
    let export = target.join("export");
    let mut symbols = BTreeMap::new();
    for entry in WalkDir::new(export).follow_links(false) {
        let entry = entry.ok()?;
        if !entry.file_type().is_file()
            || entry.path().extension().and_then(|value| value.to_str()) != Some("h")
        {
            continue;
        }
        let source = std::fs::read_to_string(entry.path()).ok()?;
        for line in source.lines() {
            let Some(rest) = line
                .trim()
                .strip_prefix("static constexpr std::string_view ")
            else {
                continue;
            };
            let (symbol, value) = rest.split_once('=')?;
            let symbol = symbol.trim();
            let value = value.trim().strip_suffix(';')?.trim();
            let value = value.strip_prefix('"')?.strip_suffix('"')?;
            if symbol.is_empty() || value.is_empty() {
                return None;
            }
            if symbols
                .insert(symbol.to_owned(), value.to_owned())
                .is_some()
            {
                return None;
            }
        }
    }
    (!symbols.is_empty()).then_some(symbols)
}

fn parse_identity_condition(line: &str) -> Option<(&str, &str)> {
    let (_, after_model) = line.split_once("identity.model_id ==")?;
    let (symbol, after_and) = after_model.split_once("&&")?;
    let symbol = symbol.trim();
    let after_weights = after_and
        .trim()
        .strip_prefix("identity.weights_id ==")?
        .trim();
    let after_quote = after_weights.strip_prefix('"')?;
    let (weights_id, suffix) = after_quote.split_once('"')?;
    if symbol.is_empty() || weights_id.is_empty() || !suffix.trim_start().starts_with(')') {
        return None;
    }
    Some((symbol, weights_id))
}

async fn canonical_regular_file(path: &Path, description: &str) -> Result<PathBuf, EngineError> {
    let canonical = tokio::fs::canonicalize(path).await.map_err(|error| {
        EngineError::InvalidConfiguration(format!(
            "{description} {} could not be resolved: {error}",
            path.display()
        ))
    })?;
    let metadata = tokio::fs::metadata(&canonical).await.map_err(|error| {
        EngineError::InvalidConfiguration(format!(
            "{description} {} could not be inspected: {error}",
            canonical.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(EngineError::InvalidConfiguration(format!(
            "{description} is not a regular file"
        )));
    }
    Ok(canonical)
}

async fn bind_ninfer_context_cost_presets(
    settings: &mut ResolvedSettings,
) -> Result<Option<serde_json::Value>, EngineError> {
    const MAXIMUM_CONTEXT_COST_PRESET_BYTES: u64 = 16 * 1024 * 1024;
    const SETTING_ID: &str = "ninfer.context_cost_presets";

    let Some(SettingValue::Path(configured)) = settings.value(SETTING_ID).cloned() else {
        return Ok(None);
    };
    let canonical = canonical_regular_file(&configured, "NInfer context-cost preset").await?;
    let id = norted_core::SettingId::new(SETTING_ID).expect("static NInfer setting ID");
    let digest = norted_core::bounded_setting_file_sha256(
        &id,
        &canonical,
        Path::new("/"),
        MAXIMUM_CONTEXT_COST_PRESET_BYTES,
    )
    .await
    .map_err(|error| EngineError::InvalidConfiguration(error.to_string()))?;
    let resolved = settings.configured.get_mut(&id).ok_or_else(|| {
        EngineError::InvalidConfiguration(
            "resolved NInfer context-cost preset disappeared while binding the file".to_owned(),
        )
    })?;
    resolved.value = SettingValue::Path(canonical.clone());
    Ok(Some(json!({
        "path": canonical,
        "sha256": digest,
    })))
}

async fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn help_contract_error(help: &str, native_arguments: &[String]) -> Option<String> {
    for required in [
        "<model.ninfer>",
        "--host",
        "--port",
        "--model-id",
        "--device",
        "--request-log-jsonl",
        "Responses/Chat",
    ] {
        if !help.contains(required) {
            return Some(format!(
                "required capability `{required}` was not advertised"
            ));
        }
    }
    native_arguments
        .iter()
        .filter(|argument| argument.starts_with("--"))
        .find(|argument| !usage_has_token(help, argument))
        .map(|argument| format!("configured native option `{argument}` was not advertised"))
}

fn usage_has_token(output: &str, expected: &str) -> bool {
    output.split_whitespace().any(|token| {
        token.trim_matches(|character: char| {
            matches!(
                character,
                '[' | ']' | '(' | ')' | '{' | '}' | '<' | '>' | ',' | '.' | ':' | ';' | '`'
            )
        }) == expected
    })
}

fn conflicts_with_managed_argument(argument: &str) -> bool {
    let argument = argument.to_ascii_lowercase().replace('_', "-");
    MANAGED_NATIVE_ARGUMENTS.iter().any(|managed| {
        argument == *managed
            || argument
                .strip_prefix(managed)
                .is_some_and(|suffix| suffix.starts_with('='))
    })
}

fn invalid_native_argument(arguments: &[String]) -> Option<&str> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if !ALLOWED_VALUE_NATIVE_ARGUMENTS.contains(&argument.as_str()) {
            return Some(argument);
        }
        let Some(value) = arguments.get(index + 1) else {
            return Some(argument);
        };
        if value.is_empty() || value.starts_with("--") {
            return Some(value);
        }
        index += 2;
    }
    None
}

fn conflicts_with_managed_environment(name: &str) -> bool {
    MANAGED_ENVIRONMENT_VARIABLES
        .iter()
        .any(|managed| name.eq_ignore_ascii_case(managed))
}

fn managed_environment_removals() -> Vec<OsString> {
    MANAGED_ENVIRONMENT_VARIABLES
        .iter()
        .map(OsString::from)
        .collect()
}

fn command_detail(stdout: &str, stderr: &str) -> String {
    match (stdout.trim(), stderr.trim()) {
        ("", "") => "no output".to_owned(),
        (stdout, "") => stdout.to_owned(),
        ("", stderr) => stderr.to_owned(),
        (stdout, stderr) => format!("{stdout}\n{stderr}"),
    }
}

fn invalid_probe(reason: String) -> EngineProbe {
    EngineProbe {
        installation: InstallationState::Invalid {
            reason: reason.clone(),
        },
        update: UpdateState::Unknown,
        healthy: false,
        detail: reason,
    }
}

fn http_endpoint(address: SocketAddr) -> String {
    if address.is_ipv6() {
        format!("http://[{}]:{}", address.ip(), address.port())
    } else {
        format!("http://{}:{}", address.ip(), address.port())
    }
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn apply_ninfer_stop_token_contract(
    definitions: &mut [norted_core::SettingDefinition],
    runtime: &InstalledRuntime,
    settings: Option<&ResolvedSettings>,
) {
    let expected = norted_engine::managed_source_overlay_sha256(ENGINE_ID, "ninfer-serve-v3");
    let speculation = settings.is_some_and(|s| matches!(s.value("ninfer.speculation"), Some(SettingValue::Toggle(true))) || matches!(s.value("ninfer.speculative_backend"), Some(SettingValue::Choice(value)) if value != "off"));
    let supported = ninfer_runtime_capabilities_for_installed(runtime).trustworthy_identity
        && runtime.manifest.identity.variant == "ninfer-serve-v3-sm120a"
        && runtime.manifest.identity.upstream_revision.as_deref()
            == Some(norted_engine::NINFER_TOKEN_STOP_REVISION)
        && !speculation
        && runtime.manifest.source_build.as_ref().is_some_and(|build| {
            build.recipe_version == "ninfer-serve-v3"
                && build.source.commit_sha == norted_engine::NINFER_TOKEN_STOP_REVISION
                && build.source_overlay_sha256 == expected
        });
    if let Some(definition) = definitions
        .iter_mut()
        .find(|d| d.id.as_str() == "ninfer.stop_token_ids")
    {
        definition.supported = supported;
        definition.unsupported_reason = (!supported).then(|| {
            "exact token stops require the Norted source overlay and speculation disabled"
                .to_owned()
        });
    }
}

#[cfg(test)]
mod tests {
    use norted_core::{ResolvedSetting, SettingId, SettingSource};

    use super::*;

    fn resolved(values: &[(&str, SettingValue)]) -> ResolvedSettings {
        ResolvedSettings {
            engine_id: ENGINE_ID.to_owned(),
            model_profile_id: None,
            configured: values
                .iter()
                .map(|(id, value)| {
                    (
                        SettingId::new(*id).expect("setting ID"),
                        ResolvedSetting {
                            value: value.clone(),
                            source: SettingSource::Invocation,
                        },
                    )
                })
                .collect(),
            effective: BTreeMap::new(),
        }
    }

    #[test]
    fn managed_source_recipe_generations_share_the_ninfer_update_line() {
        let identity = |variant: &str| norted_core::RuntimeIdentity {
            engine_id: ENGINE_ID.to_owned(),
            package_family: catalog::PACKAGE_FAMILY.to_owned(),
            version: "git-20260831-aaaaaaaa".to_owned(),
            upstream_revision: Some("a".repeat(40)),
            platform: "linux".to_owned(),
            architecture: "x86_64".to_owned(),
            accelerator: "cuda".to_owned(),
            variant: variant.to_owned(),
            package: norted_core::RuntimePackageIdentity {
                provider_id: PROVIDER_ID.to_owned(),
                repository: Some(GITHUB_REPOSITORY.to_owned()),
                release_tag: None,
                asset_id: None,
                asset_name: None,
                additional_assets: Vec::new(),
            },
        };
        let adapter = NinferAdapter::from_config(None, Path::new("."));
        for (variant, generation) in [("ninfer-serve-v1-sm120a", 1), ("ninfer-serve-v2-sm120a", 2)]
        {
            assert_eq!(
                adapter.runtime_variant_update_identity(&identity(variant)),
                RuntimeVariantUpdateIdentity {
                    functional_variant: MANAGED_NINFER_FUNCTIONAL_VARIANT.to_owned(),
                    source_recipe_generation: Some(generation),
                }
            );
        }
    }

    #[test]
    fn startup_progress_is_conservative_and_never_invents_fractions() {
        assert_eq!(parse_ninfer_startup_progress(&[]), None);
        assert_eq!(
            parse_ninfer_startup_progress(&["unrelated log line".to_owned()]),
            None
        );
        let loading = parse_ninfer_startup_progress(&["loading model weights".to_owned()])
            .expect("loading phase");
        assert_eq!(loading.phase, BackendLoadPhase::LoadingModel);
        assert_eq!(loading.fraction, None);
        let context = parse_ninfer_startup_progress(&["allocating kv cache".to_owned()])
            .expect("context phase");
        assert_eq!(context.phase, BackendLoadPhase::AllocatingContext);
    }

    #[test]
    fn native_arguments_are_strict_and_semantic_flags_are_reserved() {
        assert!(conflicts_with_managed_argument("--max-pending-requests"));
        assert!(invalid_native_argument(&["--unknown".to_owned()]).is_some());
        assert!(conflicts_with_managed_argument("--temperature=0.8"));
        assert!(conflicts_with_managed_argument("--no-thinking"));
        assert!(conflicts_with_managed_argument("--context-cost-presets"));
    }

    #[test]
    fn direct_speculation_settings_replace_named_subprofiles() {
        let off = ninfer_startup_requirements(&resolved(&[(
            "ninfer.speculation",
            SettingValue::Toggle(false),
        )]))
        .expect("off settings");
        assert_eq!(off.speculative_backend.as_deref(), Some("none"));
        assert_eq!(off.speculative_draft_window, Some(0));

        let on = ninfer_startup_requirements(&resolved(&[
            ("ninfer.speculation", SettingValue::Toggle(true)),
            (
                "ninfer.speculative_backend",
                SettingValue::Choice("mtp".to_owned()),
            ),
            ("ninfer.draft_tokens", SettingValue::UnsignedInteger(3)),
            ("ninfer.lm_head_draft", SettingValue::Toggle(true)),
        ]))
        .expect("direct MTP settings");
        assert_eq!(on.speculative_backend.as_deref(), Some("mtp"));
        assert_eq!(on.speculative_draft_window, Some(3));
        assert_eq!(on.proposal_head.as_deref(), Some("optimized"));
    }

    #[test]
    fn configured_features_require_their_capability_domain_evidence() {
        let settings = resolved(&[
            ("ninfer.thinking", SettingValue::Toggle(true)),
            ("ninfer.temperature", SettingValue::Float(0.8)),
            ("ninfer.speculation", SettingValue::Toggle(true)),
        ]);
        let no_evidence = NinferRuntimeCapabilities::default();
        assert!(validate_ninfer_settings_prelaunch(&settings, no_evidence).is_err());
        let exact = ninfer_reviewed_capabilities(true, NinferReviewedDomains::all(true));
        assert!(validate_ninfer_settings_prelaunch(&settings, exact).is_ok());
    }

    #[test]
    fn definitions_expose_actual_controls_without_an_internal_profile_selector() {
        let ids = settings::definitions()
            .into_iter()
            .map(|definition| definition.id.to_string())
            .collect::<std::collections::BTreeSet<_>>();
        for expected in [
            "ninfer.speculation",
            "ninfer.speculative_backend",
            "ninfer.draft_tokens",
            "ninfer.lm_head_draft",
            "ninfer.thinking",
            "ninfer.kv_dtype",
            "ninfer.temperature",
            "ninfer.top_p",
            "ninfer.top_k",
            "ninfer.min_p",
        ] {
            assert!(ids.contains(expected), "missing setting {expected}");
        }
        assert!(!ids.iter().any(|id| id.contains("profile")));
    }
}
