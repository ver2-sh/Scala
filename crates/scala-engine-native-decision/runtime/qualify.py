"""Bounded operational GPU qualification; never run as routine development validation.

Requires the explicit isolated_native_decision example instance. All observations
are saved under that NEW instance root. No live app paths or Link are consulted.
"""
import argparse
import ctypes
import json
import math
from pathlib import Path
import subprocess
import time
import urllib.error
import urllib.request


def request(base, path, body=None, token=None, timeout=360):
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    # Scala's typed maps serialize in key order. Send the identical ordered
    # objects to both interfaces; ordinal criteria arrays retain their order.
    req = urllib.request.Request(base + path, data=None if body is None else json.dumps(body, sort_keys=True).encode(), headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as response:
            return response.status, json.load(response)
    except urllib.error.HTTPError as error:
        return error.code, json.load(error)


def gpu():
    return subprocess.check_output(["nvidia-smi", "--query-gpu=name,driver_version,memory.used,memory.free", "--format=csv,noheader"], text=True).strip()


def process_running(pid):
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.OpenProcess.argtypes = [ctypes.c_uint32, ctypes.c_int, ctypes.c_uint32]
    kernel.OpenProcess.restype = ctypes.c_void_p
    kernel.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
    kernel.CloseHandle.argtypes = [ctypes.c_void_p]
    handle = kernel.OpenProcess(0x00100000, 0, pid)  # SYNCHRONIZE, no mutation
    if not handle:
        return False
    try:
        return kernel.WaitForSingleObject(handle, 0) == 258
    finally:
        kernel.CloseHandle(handle)


def owned_tree(pid):
    """Read a bounded Windows process snapshot; never kill by process name."""
    from ctypes import wintypes
    class Entry(ctypes.Structure):
        _fields_ = [("size", wintypes.DWORD), ("usage", wintypes.DWORD),
                    ("pid", wintypes.DWORD), ("heap", ctypes.c_size_t),
                    ("module", wintypes.DWORD), ("threads", wintypes.DWORD),
                    ("parent", wintypes.DWORD), ("priority", wintypes.LONG),
                    ("flags", wintypes.DWORD), ("name", wintypes.WCHAR * 260)]
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.CreateToolhelp32Snapshot.argtypes = [wintypes.DWORD, wintypes.DWORD]
    kernel.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    kernel.Process32FirstW.argtypes = [wintypes.HANDLE, ctypes.POINTER(Entry)]
    kernel.Process32NextW.argtypes = [wintypes.HANDLE, ctypes.POINTER(Entry)]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    snapshot = kernel.CreateToolhelp32Snapshot(2, 0)
    if snapshot == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    parents = {}
    try:
        entry = Entry()
        entry.size = ctypes.sizeof(entry)
        found = kernel.Process32FirstW(snapshot, ctypes.byref(entry))
        while found:
            parents[entry.pid] = entry.parent
            found = kernel.Process32NextW(snapshot, ctypes.byref(entry))
    finally:
        kernel.CloseHandle(snapshot)
    result = {pid}
    while added := {child for child, parent in parents.items() if parent in result} - result:
        result.update(added)
    return sorted(result)


def assert_exited(pids):
    for _ in range(40):
        if not any(process_running(pid) for pid in pids):
            return
        time.sleep(0.1)
    assert not any(process_running(pid) for pid in pids), "owned runtime process tree survived cleanup"


def finite(value):
    if isinstance(value, float):
        assert math.isfinite(value), "non-finite native observation"
    elif isinstance(value, dict):
        for item in value.values():
            finite(item)
    elif isinstance(value, list):
        for item in value:
            finite(item)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("instance", type=Path)
    parser.add_argument("profile")
    args = parser.parse_args()
    access = json.loads((args.instance / "access.json").read_text())
    report = {"profile": args.profile, "comparison_policy": "exact equality of every typed answer field; upstream calibration_version is recorded separately, with no numerical tolerance", "gpu_before": gpu()}
    def public(path, body=None):
        started = time.monotonic()
        result = request(access["public_endpoint"], path, body, access["api_key"])
        if path.startswith("/v1/models"):
            report.setdefault("model_listing_elapsed_seconds", []).append({"path": path, "seconds": time.monotonic() - started})
        return result
    def control(path, body=None):
        return request(access["control_endpoint"], "/control/v1/" + path, body, access["control_token"], timeout=30)
    def save():
        (args.instance / "qualification.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    body = {"model": args.profile,
            "state": {"message": "A customer asks for help resetting a password. There is no purchase request or urgent safety issue."},
            "questions": {
                "route": {"type": "choice", "instructions": "Choose the department that should handle the customer request.", "criteria": {"support": "Account and technical assistance", "sales": "Purchases and pricing"}},
                "urgency": {"type": "score", "instructions": "Rate the urgency of the customer request using the ordered criteria.", "criteria": ["Routine", "Urgent"]},
                "account_support": {"type": "noul", "instructions": "Is this request for account support?"}}}
    report["request"] = body
    pid = None
    shutdown = False
    try:
        report["models_before"] = public("/v1/models")[1]
        report["decisions_before"] = public("/v1/models?output_modalities=decisions")[1]
        candidate = next(m for m in report["models_before"]["data"] if m["id"] == args.profile)
        assert candidate["capabilities"]["decision_candidate"] and not candidate["capabilities"].get("decision", False)
        assert candidate["architecture"]["output_modalities"] == ["decisions"]
        # An authenticated valid request must JIT-load and qualify through the
        # real manager. There is no direct launch or execution-proof injection.
        status, scala = public("/v1/systemone", body)
        report["scala_status"], report["scala"] = status, scala
        report["status_loaded"] = control("status")[1]
        save()
        assert status == 200, "typed Decision request failed"
        backend = next(b for b in report["status_loaded"]["backends"] if b["model_profile_id"] == args.profile)
        pid = backend["process_id"]
        endpoint = backend["private_endpoint"]
        report["health"] = request(endpoint, "/health")[1]
        if backend["runtime_variant"] == "torch-readout":
            report["execution"] = request(endpoint, "/scala/execution")[1]
            assert report["execution"]["launch_nonce"] == report["health"]["launch_nonce"]
            assert report["execution"]["execution_device"] == "cuda"
            assert report["execution"]["cuda_allocated_bytes"] > 0
        assert backend["runtime_variant"] in {"torch-readout", "vllm-labels"}
        native_body = {k: body[k] for k in ("state", "questions")}
        native_status, native = request(endpoint, "/v1/systemone", native_body)
        report["native_status"], report["native"] = native_status, native
        assert native_status == 200
        typed_native = {name: {key: value for key, value in answer.items() if key != "calibration_version"}
                        for name, answer in native["answers"].items()}
        report["native_calibration_versions"] = {name: answer["calibration_version"]
            for name, answer in native["answers"].items() if "calibration_version" in answer}
        assert scala["answers"] == typed_native, "native/Scala typed answers differ under the predeclared exact comparison"
        assert set(scala["answers"]) == set(body["questions"])
        for name, question in body["questions"].items():
            answer = scala["answers"][name]
            assert answer["type"] == question["type"]
            if question["type"] == "choice":
                assert set(answer["probabilities"]) == set(question["criteria"])
                assert answer["choice"] in question["criteria"]
            if question["type"] == "score":
                assert set(answer["probabilities"]) == {"0", "1"}
                assert set(answer["legend"]) == {"0", "1"}
            finite(answer)
        report["models_loaded"] = public("/v1/models")[1]
        report["decisions_loaded"] = public("/v1/models?output_modalities=decisions")[1]
        qualified = next(m for m in report["models_loaded"]["data"] if m["id"] == args.profile)
        assert qualified["capabilities"]["decision"]
        for key in ("chat_completions", "completions", "embeddings", "streaming"):
            assert not qualified["capabilities"].get(key, False)
        report["gpu_loaded"] = gpu()
        report["owned_processes"] = owned_tree(pid)
        report["unload"] = control("unload", {"model_profile_id": args.profile})
        assert report["unload"][0] == 200
        assert_exited(report["owned_processes"])
        report["gpu_unloaded"] = gpu()
        report["models_unloaded"] = public("/v1/models")[1]
        unloaded = next(m for m in report["models_unloaded"]["data"] if m["id"] == args.profile)
        assert unloaded.get("capabilities", {}).get("decision_candidate", False)
        assert not unloaded["capabilities"].get("decision", False)
        save()
        # A separate explicit load owns no inference lease. Cancel its actual
        # runtime during startup, using the existing control unload semantics.
        # Metadata probes belonging to concurrent status/discovery requests are
        # not that runtime's children and must not be mistaken for orphans.
        report["cancel_load"] = control("load", {"model_profile_id": args.profile})
        assert report["cancel_load"][0] == 202
        starting = None
        for _ in range(240):
            state = control("status")[1]
            starting = next((b for b in state["backends"] if b["model_profile_id"] == args.profile), None)
            if starting and starting.get("process_id"):
                break
            if starting and starting["lifecycle"] == "failed":
                break
            time.sleep(0.5)
        assert starting and starting["lifecycle"] == "loading" and starting.get("process_id"), "did not observe cancellable runtime startup"
        report["cancel_starting"] = starting
        report["cancel_owned_processes"] = owned_tree(starting["process_id"])
        report["cancel_unload"] = control("unload", {"model_profile_id": args.profile})
        assert report["cancel_unload"][0] == 200
        assert_exited(report["cancel_owned_processes"])
        report["gpu_cancelled"] = gpu()
        save()
        # One subsequent JIT request exercises reuse of the same immutable
        # runtime after proof invalidation, with a fresh process/launch nonce.
        status, again = public("/v1/systemone", body)
        report["subsequent_jit"] = {"status": status, "response": again, "control": control("status")[1]}
        assert status == 200
        second = next(b for b in report["subsequent_jit"]["control"]["backends"] if b["model_profile_id"] == args.profile)
        assert second["process_id"] != pid
        report["subsequent_health"] = request(second["private_endpoint"], "/health")[1]
        assert report["subsequent_health"]["launch_nonce"] != report["health"]["launch_nonce"]
        report["shutdown_owned_processes"] = owned_tree(second["process_id"])
        (args.instance / "stop").touch()
        for _ in range(120):
            if not (args.instance / "access.json").exists():
                break
            time.sleep(0.25)
        assert not (args.instance / "access.json").exists(), "isolated Scala shutdown did not complete"
        assert_exited(report["shutdown_owned_processes"])
        shutdown = True
        report["gpu_shutdown"] = gpu()
        report["status"] = "passed_smoke_unload_cancel_reload_shutdown"
    except BaseException as error:
        report["status"] = "incomplete"
        report["error"] = repr(error)
        save()
        raise
    finally:
        try:
            if not shutdown:
                report["final_unload"] = control("unload", {"model_profile_id": args.profile})
                report["final_control"] = control("status")[1]
            report["gpu_final"] = gpu()
        except Exception as error:
            report["cleanup_error"] = repr(error)
        save()
        (args.instance / "stop").touch()


if __name__ == "__main__":
    main()
