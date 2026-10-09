#!/usr/bin/env python3
"""Read-only, offline source audit plus synthetic Scala -> NInfer CPU tests.

Requires explicit source roots. Does not import the Studio application, inspect
saved chats/settings, load artifacts, run inference, or alter installed sources.
Build products exist only in a temporary directory. This verifies the reviewed
v2 contract; it does not qualify another runtime or an arbitrary model template.
"""
import argparse
import ast
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
from types import SimpleNamespace
from typing import Any, Optional


REPO = Path(__file__).resolve().parent.parent


def checked_run(command, **kwargs):
    subprocess.run(command, check=True, timeout=180, **kwargs)


def compile_functions(path, names, namespace):
    tree = ast.parse(path.read_text())
    selected = [node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name in names]
    assert {node.name for node in selected} == set(names)
    # Execute exact installed function bodies, avoiding the application's heavy
    # imports/startup hooks. All inputs below are generated synthetic text.
    exec(compile(ast.Module(body=selected, type_ignores=[]), str(path), "exec"), namespace)


def unsloth_contract(backend):
    namespace = {"Any": Any, "Optional": Optional, "json": json, "_hashlib": hashlib}
    compile_functions(backend / "core/inference/message_content.py", ["named_turn"], namespace)
    for role in ["system", "developer", "user", "assistant"]:
        turn = {"role": role, "content": "synthetic"}
        assert namespace["named_turn"](turn, {"name": "participant"})["name"] == "participant"

    routes = backend / "routes/inference.py"
    assignments = {
        "_INPUT_DOCUMENT_PROVIDERS", "_MINTED_TOOL_CALL_ID_SUFFIX", "_MISTRAL_TOOL_CALL_ID",
        "_ANTHROPIC_TOOL_CALL_ID", "_ANTHROPIC_TOOL_CALL_ID_ILLEGAL",
    }
    namespace["_re"] = re
    selected = [node for node in ast.parse(routes.read_text()).body
                if isinstance(node, ast.Assign) and any(isinstance(target, ast.Name) and target.id in assignments
                                                       for target in node.targets)]
    assert len(selected) == len(assignments)
    exec(compile(ast.Module(body=selected, type_ignores=[]), str(routes), "exec"), namespace)
    # Image promotion is deliberately outside this text-only audit. Its input
    # is captured here before promotion so the builder's name handling is tested
    # directly, without executing unrelated image/storage code.
    namespace["promote_mcp_history_images"] = lambda messages, **kwargs: messages
    compile_functions(routes, ["_replay_tool_call_id_map", "_replay_tool_call_ids", "_build_external_messages"], namespace)

    def message(role, name, content="synthetic", **kwargs):
        return SimpleNamespace(role=role, name=name, content=content, reasoning_content=None,
                               tool_calls=kwargs.get("tool_calls"), tool_call_id=kwargs.get("tool_call_id"), extra_content=None)

    builder = namespace["_build_external_messages"]
    for role in ["system", "developer", "user", "assistant"]:
        for content in ["synthetic", [SimpleNamespace(type="text", text="synthetic")]]:
            for vision in [False, True]:
                built = builder([message(role, "participant", content)], vision, provider_type="custom")
                assert len(built) == 1 and built[0]["role"] == role
                assert "name" not in built[0], "external-builder behavior changed; re-audit identity preservation"
    for tool in ["web_search", "edit_file", "python"]:
        calls = [{"id": "synthetic-call", "type": "function", "function": {"name": tool, "arguments": "{}"}}]
        built = builder([message("user", "coder"), message("assistant", "helper", None, tool_calls=calls),
                         message("tool", tool, tool_call_id="synthetic-call")], False, provider_type="custom")
        assert [item.get("name") for item in built] == [None, None, tool]
        assert built[1]["tool_calls"][0]["id"] == built[2]["tool_call_id"] == "synthetic-call"
    print("Unsloth synthetic audit: named_turn retains supplied identities; external builder omits participant names and retains tool names.", flush=True)
    for relative in ["core/inference/message_content.py", "core/inference/inference.py", "core/inference/chat_template_helpers.py",
                     "core/inference/tool_loop_controller.py", "core/inference/studio_tool_loop.py", "core/inference/external_provider.py",
                     "routes/inference.py", "models/inference.py"]:
        print(relative, hashlib.sha256((backend / relative).read_bytes()).hexdigest(), flush=True)


def native_contract(source, output, native_includes):
    adapter = (REPO / "crates/scala-engine-ninfer/src/lib.rs").read_text()
    reviewed = adapter.split("const REVIEWED_SOURCE_BLOBS:", 1)[1].split("\n];", 1)[0]
    owners = re.findall(r'\("([^"]+)", "([0-9a-f]{40})"\)', reviewed)
    assert len(owners) == 97
    for relative, expected in owners:
        contents = (source / relative).read_bytes()
        actual = hashlib.sha1(f"blob {len(contents)}\0".encode() + contents).hexdigest()
        assert actual == expected, f"unreviewed NInfer source owner: {relative}"
    print("NInfer: all 97 Scala-reviewed source owners match; no capability qualification changed.", flush=True)

    frontend = (source / "src/targets/qwen3_6/impl/frontend/frontend.cpp").read_text()
    # Compile the exact native message-conversion function in isolation: the
    # full frontend also owns tokenizer/media code which this CPU test excludes.
    conversion = frontend.split("std::vector<fi::ChatMessage> convert_messages(", 1)[1]
    conversion = "std::vector<fi::ChatMessage> convert_messages(" + conversion.split("\nfi::ChatRenderOptions render_options", 1)[0]
    (output / "native-conversion.inc").write_text(
        "namespace ninfer::targets::qwen3_6 {\nnamespace fi = frontend_internal;\n" + conversion + "\n}\n")
    sources = ["src/serve/openai_chat_request.cpp", "src/serve/openai_common.cpp", "src/serve/request_validation.cpp",
               "src/serve/translate.cpp", "src/targets/qwen3_6/impl/frontend/chat_template.cpp",
               "src/targets/qwen3_6/impl/frontend/digest.cpp"]
    executable = output / "native-contract"
    command = ["c++", "-std=c++20", "-O0", "-ffunction-sections", "-fdata-sections", "-Wl,--gc-sections"]
    for include in [source / "src", source / "include", source / "src/targets/qwen3_6/export", source / "third_party", output, *native_includes]:
        command.extend(["-I", str(include)])
    command.extend([str(REPO / "scripts/unsloth-native-contract.cpp"), *(str(source / item) for item in sources), "-o", str(executable)])
    checked_run(command)
    env = {**os.environ, "SCALA_TEST_NINFER_CONTRACT_BIN": str(executable), "SCALA_TEST_NINFER_SOURCE": str(source)}
    checked_run(["cargo", "test", "--offline", "-p", "scala-api", "unsloth_"], cwd=REPO, env=env)
    checked_run(["cargo", "test", "--offline", "-p", "scala-engine", "unsloth_"], cwd=REPO)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ninfer-source", required=True, type=Path)
    parser.add_argument("--unsloth-backend", required=True, type=Path)
    parser.add_argument("--native-include", action="append", default=[], type=Path,
                        help="Additional header directory (e.g. the installed CUDA/NVTX includes); no GPU libraries are linked.")
    args = parser.parse_args()
    unsloth_contract(args.unsloth_backend.resolve())
    with tempfile.TemporaryDirectory(prefix="scala-unsloth-contract-") as temporary:
        native_contract(args.ninfer_source.resolve(), Path(temporary), args.native_include)


if __name__ == "__main__":
    main()
