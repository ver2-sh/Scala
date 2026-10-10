// CPU-only source contract harness. No Engine, tokenizer, artifact, GPU or HTTP
// service is linked or started. Input comes exclusively from synthetic tests.
#include "serve/openai_chat.h"
#include "serve/translate.h"
#include "targets/qwen3_6/impl/frontend/chat_template.h"
#include "native-conversion.inc"

#include <fstream>
#include <iostream>
#include <iterator>

namespace fi = ninfer::targets::qwen3_6::frontend_internal;
using Json = ninfer::serve::RequestJson;

const char* role_name(ninfer::ChatRole role) {
    switch (role) {
        case ninfer::ChatRole::System: return "system";
        case ninfer::ChatRole::Developer: return "developer";
        case ninfer::ChatRole::User: return "user";
        case ninfer::ChatRole::Assistant: return "assistant";
        case ninfer::ChatRole::Tool: return "tool";
    }
    throw std::logic_error("invalid synthetic role");
}

int main(int argc, char** argv) {
    if (argc != 3) return 2;
    const Json envelope = Json::parse(std::cin);
    try {
        std::ifstream template_file(envelope.at("template") == "thinking_toggle" ? argv[1] : argv[2]);
        std::string source((std::istreambuf_iterator<char>(template_file)), {});
        // Match NInfer's own read_template_fixture test helper exactly.
        if (!source.empty() && source.back() == '\n') source.pop_back();
        const auto renderer = fi::CompiledChatTemplate::resolve(source);
        const auto parsed = ninfer::serve::parse_chat_completion_request(envelope.at("body"), {});
        const auto semantics = ninfer::serve::resolve_prompt_semantics(parsed.generation, {}, renderer.capabilities());
        auto prompt = ninfer::serve::to_prompt_input(parsed.generation, semantics, {});
        auto messages = ninfer::targets::qwen3_6::convert_messages(std::move(prompt.messages));
        fi::ChatRenderOptions options;
        options.enable_thinking = prompt.options.enable_thinking;
        options.reasoning_effort = prompt.options.reasoning_effort;
        options.preserve_thinking = prompt.options.preserve_thinking;
        options.tool_jsons = prompt.options.tool_jsons;
        options.continuation = prompt.options.continuation;
        Json serialized = Json::array();
        for (const auto& message : messages) {
            Json out{{"role", role_name(message.role)}, {"content", message.rendered_content().text}};
            if (!message.tool_call_id.empty()) out["tool_call_id"] = message.tool_call_id;
            if (!message.reasoning_content.empty()) out["reasoning_content"] = message.reasoning_content;
            if (!message.tool_calls.empty()) {
                out["tool_calls"] = Json::array();
                for (const auto& call : message.tool_calls) {
                    out["tool_calls"].push_back(Json{{"id", call.id}, {"type", "function"},
                        {"function", Json{{"name", call.name}, {"arguments", call.arguments_json}}}});
                }
            }
            serialized.push_back(std::move(out));
        }
        std::cout << Json{{"messages", serialized}, {"rendered", renderer.render(messages, options).text}}.dump();
    } catch (const ninfer::serve::ApiException& error) {
        // Return only structural diagnostics; never print exception messages.
        std::cout << Json{{"error_code", error.error().code}, {"status", error.error().status}}.dump();
    } catch (const std::exception&) {
        return 3;
    }
}
