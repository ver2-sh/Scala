#!/usr/bin/env python3
"""H2O-Lightning-4B shim: typed decisions over a stock vLLM server, one output token per decision. Stdlib only.

WHAT IT DOES
    POST /v1/systemone  (the `typesafe` decision wire format; /api/alpha/decisions is an alias)
      -> renders each question in the prompt the model was trained on (serve_config.json: the chat template with
         thinking off, the system prompt, the "plain" layout, compact JSON for a structured state, one label per
         option, the "Answer:" prefill)
      -> sends ONE vLLM completion per question with max_tokens=1 and `logprob_token_ids` set to that question's
         label tokens, so every option is read at the answer slot and none can fall out of a top-k
      -> applies one temperature for every question type, then the yes/no commit floor
      -> returns {"model", "answers", "probability_source", "effort_used", "usage"}

    Several questions about one state are sent concurrently; vLLM's prefix cache shares the state between them, and
    usage.input_tokens counts that shared head once plus each question's own tail.
    It generates no text and calls nothing but the local vLLM server.

    python3 h2o_lightning_shim.py --config serve_config.json --vllm http://127.0.0.1:8000 --port 8741

STATUS CODES (an evaluation runner stops after three consecutive failures unless a failure is a 422): a request this
system cannot answer -- too many options, a malformed question, a prompt over vLLM's context -- is a 422, scored as
one wrong answer; an empty request is a 400; an unreachable or failing vLLM is a 502, and a vLLM that does not serve
the configured model is a 503, so a broken backend stops a run; vLLM's own 401/403/429 pass through.
"""
from __future__ import annotations

import argparse
import http.client
import json
import math
import os
import re
import sys
import threading
import time
import urllib.parse
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SHIM_VERSION = "h2o-lightning-shim"
PRIMITIVES = ("choice", "noul", "score")
EFFORT_TIERS = ("fast", "balanced", "thorough")
# vLLM 0.30 accepts at most 128 `logprob_token_ids` per request; a question with more options is read in chunks
LOGPROB_CHUNK = int(os.environ.get("SHIM_LOGPROB_CHUNK", "128"))
TIMEOUT = float(os.environ.get("SHIM_TIMEOUT", "180"))
# vLLM's wording when a prompt does not fit --max-model-len (it has changed between releases)
_CONTEXT_ERROR = re.compile(r"maximum context length|maximum model length|max_model_len|too long and exceeds|"
                            r"longer than the maximum", re.I)
# Qwen3.5's chat template for a system and a user message with the generation prompt and thinking off; the template
# trims both contents (Jinja's `trim` is Python's str.strip)
CHATML = {"system": "<|im_start|>system\n{content}<|im_end|>\n",
          "user": "<|im_start|>user\n{content}<|im_end|>\n",
          "generation": "<|im_start|>assistant\n<think>\n\n</think>\n\n"}


class Unprocessable(ValueError):
    """A request in the contract's shape that this system cannot answer: a 422, one wrong item, the run goes on."""


class BadRequest(ValueError):
    """Not the contract's shape at all (no questions, an unknown effort tier): a 400."""


class UpstreamError(RuntimeError):
    """vLLM failed or could not be reached: a 502 (or 503, or vLLM's own 401/403/429), so a run stops."""

    def __init__(self, message, status=502):
        super().__init__(message)
        self.status = status


# ------------------------------------------------------------------------------------------------- the prompt
def render_state(state, compact):
    """The record: a string verbatim, anything else as JSON (compact when the config says so)."""
    if isinstance(state, str):
        return state
    if compact:
        return json.dumps(state, ensure_ascii=False, separators=(",", ":"))
    return json.dumps(state, indent=1)


def desc_text(v):
    """An option description or score level: a string verbatim, anything else as JSON."""
    return v if isinstance(v, str) else json.dumps(v, ensure_ascii=False)


def render_instr(value):
    """`instructions` may be a string, an object or an array."""
    return value if isinstance(value, str) else json.dumps(value, indent=1)


def options_of(question, max_options=255):
    """(option names, descriptions) for the three question types. A yes/no question is always [true, false]."""
    typ = question.get("type", "choice")
    criteria = question.get("criteria")
    if typ not in PRIMITIVES:
        raise Unprocessable(f"question type {typ!r} is not one of {list(PRIMITIVES)}")
    if typ == "noul":
        criteria = criteria or {}
        if not isinstance(criteria, dict):
            raise Unprocessable("noul criteria must be an object with true/false descriptions")
        true_desc = criteria.get("true", criteria.get("yes"))
        false_desc = criteria.get("false", criteria.get("no"))
        return ["true", "false"], [
            desc_text(true_desc) if true_desc not in (None, "") else "the statement holds",
            desc_text(false_desc) if false_desc not in (None, "") else "it does not",
        ]
    if typ == "score":
        if not isinstance(criteria, list) or len(criteria) < 1:
            raise Unprocessable("score criteria must be an ordered array of levels")
        return [str(i) for i in range(len(criteria))], [desc_text(value) for value in criteria]
    if not isinstance(criteria, dict) or len(criteria) < 1:
        raise Unprocessable("choice criteria must map option -> description")
    if len(criteria) > max_options:
        raise Unprocessable(f"choice supports at most {max_options} options")
    names = list(criteria)
    return names, [desc_text(criteria[name]) if criteria[name] is not None else name for name in names]


def option_lines(labels, names, descs):
    """One line per option: `A) name: description`."""
    if len(labels) < len(names):
        raise Unprocessable(f"{len(names)} options but only {len(labels)} labels")
    return [f"{l}) {n}: {d}" for l, n, d in zip(labels, names, descs)]


def user_turn(state_text, instructions, names, descs, labels, layout="plain"):
    """The user message."""
    body = "\n".join(option_lines(labels, names, descs))
    if layout == "plain":
        return f"record: {state_text}\nquestion: {instructions}\noptions:\n{body}"
    if layout == "markdown":
        return f"### RECORD\n{state_text}\n\n### QUESTION\n{instructions}\n\n### OPTIONS\n{body}\n"
    raise ValueError(f"layout {layout!r} is not supported")


def render_chat(system, user, prefill, chat=True):
    """The full prompt: the chat template for [system, user] with thinking off, then the answer prefill. Without the
    template, the same parts joined by blank lines."""
    if not chat:
        return "\n\n".join(([] if system is None else [system]) + [user]) + f"\n{prefill}"
    out = "" if system is None else CHATML["system"].replace("{content}", system.strip())
    return out + CHATML["user"].replace("{content}", user.strip()) + CHATML["generation"] + prefill


# ------------------------------------------------------------------------------------------------- the answer
def confidence(p):
    """Normalized maximum: 1 for a point mass, 0 for a uniform distribution."""
    n = len(p)
    if n < 2:
        return 1.0
    return float((max(p) - 1.0 / n) / (1.0 - 1.0 / n))


def probabilities(logprobs, temperature):
    """softmax(log-probabilities / T) over the options."""
    if not logprobs or not all(isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v)
                               for v in logprobs):
        raise UpstreamError("vLLM returned an invalid log-probability vector")
    v = [x / max(float(temperature), 1e-3) for x in logprobs]
    m = max(v)
    e = [math.exp(x - m) for x in v]
    s = sum(e)
    if not math.isfinite(s) or s <= 0:
        raise UpstreamError("vLLM returned log-probabilities that cannot be normalized")
    return [x / s for x in e]


def commit_noul(p_yes, floor):
    """A yes/no probability with max(P, 1-P) under the floor moves to the floor on its own side (a tie goes to yes).
    The answer never changes; 0 turns the floor off."""
    if not floor:
        return p_yes
    if max(p_yes, 1.0 - p_yes) >= floor:
        return p_yes
    return floor if p_yes >= 0.5 else 1.0 - floor


def format_answer(typ, names, descs, logprobs, temperature, noul_floor, threshold=None):
    """One question's answer in the wire shape."""
    p = probabilities(logprobs, temperature)
    if typ == "noul":
        py = commit_noul(float(p[0]), noul_floor)
        answer = {"type": "noul", "noul": float(py)}
        if threshold is not None:
            answer["threshold"] = float(threshold)
            answer["decision"] = bool(py >= float(threshold))
        return answer
    if typ == "score":
        return {
            "type": "score",
            "score": float(sum(i * x for i, x in enumerate(p))),
            "legend": {str(i): d for i, d in enumerate(descs)},
            "probabilities": {str(i): float(x) for i, x in enumerate(p)},
            "confidence": confidence(p),
        }
    k = max(range(len(p)), key=p.__getitem__)          # the first maximum
    return {
        "type": "choice",
        "choice": names[k],
        "probabilities": {name: float(x) for name, x in zip(names, p)},
        "confidence": confidence(p),
    }


class Contract:
    """The serving settings, from serve_config.json."""

    def __init__(self, cfg, env=os.environ):
        p = cfg["prompt"]
        self.cfg = cfg
        self.model = cfg["model"]
        self.chat = bool(p.get("chat_template", True))
        if p.get("enable_thinking", False):
            raise ValueError("prompt.enable_thinking must be false: the answer is read after a closed think block")
        self.system = p.get("system") or None
        self.layout = p.get("layout", "plain")
        self.state_compact = bool(p.get("state_compact", True))
        self.prefill = p.get("prefill", "Answer:")
        self.max_state_tokens = int(p.get("max_state_tokens", 32000))
        self.default_instructions = p.get("default_instructions", "Answer the question below.")
        if not isinstance(cfg.get("labels"), list):
            raise ValueError("serve_config.json: `labels` must be the list of option labels, in order")
        self.labels = [str(l) for l in cfg["labels"]][: int(cfg.get("max_options", len(cfg["labels"])))]
        if len(set(self.labels)) != len(self.labels):
            raise ValueError("serve_config.json: the labels are not distinct")
        self.label_ids = None             # each label's token id at the answer slot, read from vLLM's tokenizer
        # SHIM_TEMPERATURE / SHIM_NOUL_FLOOR override the config; SHIM_NOUL_FLOOR=0 turns the floor off
        t_env, f_env = env.get("SHIM_TEMPERATURE", "").strip(), env.get("SHIM_NOUL_FLOOR", "").strip()
        self.temperature = float(t_env) if t_env else float(cfg["temperature"])
        self.temperature_source = "SHIM_TEMPERATURE" if t_env else "serve_config.json"
        self.noul_floor = float(f_env) if f_env else float(cfg["noul_floor"])
        self.noul_floor_source = "SHIM_NOUL_FLOOR" if f_env else "serve_config.json"
        if not 0.05 <= self.temperature <= 20.0:
            raise ValueError(f"temperature {self.temperature} is outside [0.05, 20]")
        if self.noul_floor and not 0.5 < self.noul_floor < 1.0:
            raise ValueError(f"noul_floor {self.noul_floor}: must be in (0.5, 1), or 0 to turn it off")

    def question_prompt(self, state_text, q, qid="q"):
        """(prompt text, option names, descriptions, labels) for one question."""
        names, descs = options_of(q, len(self.labels))
        if len(names) > len(self.labels):
            raise Unprocessable(f"question {qid}: {len(names)} options exceeds {len(self.labels)}")
        labels = self.labels[:len(names)]
        instr = render_instr(q.get("instructions", "")).lstrip() or self.default_instructions
        user = user_turn(state_text, instr, names, descs, labels, self.layout)
        return render_chat(self.system, user, self.prefill, self.chat), names, descs, labels


# ------------------------------------------------------------------------------------------------- vLLM client
class VLLM:
    """A keep-alive HTTP/1.1 connection per thread to one vLLM server."""

    def __init__(self, base, model):
        u = urllib.parse.urlsplit(base if "://" in base else "http://" + base)
        self.base = f"{u.scheme}://{u.netloc}"
        self.https = u.scheme == "https"
        self.host = u.netloc
        self.model = model
        self.local = threading.local()

    def _conn(self, fresh=False):
        c = getattr(self.local, "conn", None)
        if c is None or fresh:
            if c is not None:
                c.close()
            cls = http.client.HTTPSConnection if self.https else http.client.HTTPConnection
            c = self.local.conn = cls(self.host, timeout=TIMEOUT)
        return c

    def call(self, method, path, body=None):
        """(status, parsed JSON or text). A connection the server closed is reopened once."""
        data = None if body is None else json.dumps(body).encode()
        headers = {"Content-Type": "application/json"} if data is not None else {}
        for attempt in (0, 1):
            try:
                c = self._conn(fresh=attempt == 1)
                c.request(method, path, body=data, headers=headers)
                r = c.getresponse()
                raw = r.read()
                break
            except (http.client.HTTPException, OSError) as e:
                if attempt == 1:
                    raise UpstreamError(f"vLLM at {self.base} unreachable: {type(e).__name__}: {e}") from e
        try:
            return r.status, json.loads(raw)
        except ValueError:
            return r.status, raw.decode("utf-8", "replace")

    def post(self, path, body):
        status, out = self.call("POST", path, body)
        if status == 200 and isinstance(out, dict):
            return out
        detail = json.dumps(out)[:500] if isinstance(out, dict) else str(out)[:500]
        if status in (400, 413) and _CONTEXT_ERROR.search(detail):
            raise Unprocessable(f"over the server's context limit: {detail}")
        if status == 400:
            raise Unprocessable(f"vLLM refused the request: {detail}")
        raise UpstreamError(f"vLLM {path} HTTP {status}: {detail}", status if status in (401, 403, 429) else 502)

    def tokenize(self, text):
        return self.post("/tokenize", {"model": self.model, "prompt": text, "add_special_tokens": False})["tokens"]

    def detokenize(self, ids):
        return self.post("/detokenize", {"model": self.model, "tokens": ids})["prompt"]

    def label_logprobs(self, prompt, ids):
        """{token id: log-probability} at the answer slot, one completion per LOGPROB_CHUNK labels."""
        out, usage = {}, None
        for k in range(0, len(ids), LOGPROB_CHUNK):
            chunk = ids[k:k + LOGPROB_CHUNK]
            r = self.post("/v1/completions", {
                "model": self.model, "prompt": prompt, "max_tokens": 1, "temperature": 0.0,
                "logprobs": len(chunk), "logprob_token_ids": chunk, "return_tokens_as_token_ids": True,
                "add_special_tokens": False})
            try:
                top = r["choices"][0]["logprobs"]["top_logprobs"][0]
                got = {int(key.split(":", 1)[1]): float(v) for key, v in top.items()}
            except (KeyError, IndexError, TypeError, ValueError) as e:
                raise UpstreamError(f"vLLM returned no label log-probabilities at the answer slot ({e})") from e
            missing = [i for i in chunk if i not in got]
            if missing:
                raise UpstreamError(f"vLLM omitted label tokens {missing[:5]} from logprob_token_ids")
            out.update({i: got[i] for i in chunk})
            usage = usage or r.get("usage") or {}
        return out, usage


def common_prefix_len(seqs):
    """The length of the longest common prefix of several token id lists; 0 for fewer than two."""
    if len(seqs) < 2:
        return 0
    n = min(len(x) for x in seqs)
    for i in range(n):
        if any(x[i] != seqs[0][i] for x in seqs[1:]):
            return i
    return n


# ------------------------------------------------------------------------------------------------------ server
class Shim:
    def __init__(self, contract, vllm, min_context=4096):
        self.c = contract
        self.v = vllm
        self.min_context = min_context
        self.ready = None                 # the backend check, done once at first use (vLLM may start later)
        self.ready_lock = threading.Lock()
        self.max_model_len = None
        self.pool = ThreadPoolExecutor(max_workers=int(os.environ.get("SHIM_QUESTION_THREADS", "64")))

    def check_backend(self):
        """vLLM serves the configured model with a usable context, and every label is ONE token at the answer slot:
        after a probe prompt with all the labels, appending " <label>" adds exactly one token, whose id is read."""
        if self.ready:
            return
        with self.ready_lock:
            if self.ready:
                return
            status, out = self.v.call("GET", "/v1/models")
            models = (out.get("data") or []) if isinstance(out, dict) else []
            entry = next((m for m in models if isinstance(m, dict) and m.get("id") == self.c.model), None)
            if status != 200 or entry is None:
                raise UpstreamError(f"vLLM at {self.v.base} does not serve {self.c.model!r} "
                                    f"(start it with --served-model-name {self.c.model})", 503)
            self.max_model_len = entry.get("max_model_len")
            if isinstance(self.max_model_len, int) and self.max_model_len < self.min_context:
                raise UpstreamError(f"vLLM's max_model_len {self.max_model_len} is below {self.min_context}", 503)
            n = len(self.c.labels)
            probe = {"type": "choice", "instructions": "Which?", "criteria": {f"o{i}": f"d{i}" for i in range(n)}}
            text = self.c.question_prompt(render_state({"x": 1}, self.c.state_compact), probe)[0]
            base = self.v.tokenize(text)
            ids, bad = {}, []
            for label in self.c.labels:
                full = self.v.tokenize(text + " " + label)
                if len(full) != len(base) + 1 or full[:len(base)] != base:
                    bad.append(label)
                else:
                    ids[label] = full[-1]
            if bad or len(set(ids.values())) != len(ids):
                raise UpstreamError(f"these labels are not one distinct token at the answer slot under vLLM's "
                                    f"tokenizer: {bad[:8]} (is this serve_config.json the one for this model?)", 503)
            self.c.label_ids = ids
            self.ready = True
            sys.stderr.write(f"shim: vLLM serves {self.c.model} (max_model_len {self.max_model_len}); "
                             f"{n} labels verified\n")

    def truncate_state(self, text):
        """A record over max_state_tokens keeps its head and its tail around a "..." line (vLLM's tokenizer)."""
        mx = self.c.max_state_tokens
        if not mx or len(text.encode("utf-8", "ignore")) <= mx:      # a byte-level BPE never has more tokens than bytes
            return text
        ids = self.v.tokenize(text)
        if len(ids) <= mx:
            return text
        half = mx // 2
        return self.v.detokenize(ids[:half]) + "\n...\n" + self.v.detokenize(ids[-half:])

    def one(self, prompt, labels):
        ids = [self.c.label_ids[l] for l in labels]
        lp, usage = self.v.label_logprobs(prompt, ids)
        return [lp[i] for i in ids], usage

    def decide(self, body):
        """The /v1/systemone handler. Raises BadRequest / Unprocessable / UpstreamError."""
        t0 = time.time()
        if not isinstance(body, dict) or "state" not in body or not isinstance(body.get("questions"), dict):
            raise Unprocessable("expected a JSON object with `state` and a `questions` object")
        questions = body["questions"]
        if not questions:
            raise BadRequest("no questions")
        if not all(isinstance(q, dict) for q in questions.values()):
            raise Unprocessable("every question must be an object")
        effort = body.get("effort")
        if effort is not None and not isinstance(effort, (str, dict)):
            raise Unprocessable("effort must be a tier name or an object")
        asked = effort if isinstance(effort, str) or effort is None else effort.get("tier", "fast")
        if asked is not None and asked not in EFFORT_TIERS:
            raise BadRequest(f"effort tier {asked!r} is not one of {list(EFFORT_TIERS)}")
        self.check_backend()
        state_txt = self.truncate_state(render_state(body["state"], self.c.state_compact))
        work = []
        for qid, q in questions.items():
            text, names, descs, labels = self.c.question_prompt(state_txt, q, qid)
            thr = None
            if q.get("type", "choice") == "noul":
                t = q.get("threshold", q.get("noul_threshold"))
                if isinstance(t, (int, float)) and 0.0 <= float(t) <= 1.0:
                    thr = float(t)
            work.append((qid, q.get("type", "choice"), names, descs, text, labels, thr))
        shared = 0
        if len(work) == 1:
            reads = [self.one(work[0][4], work[0][5])]
        else:                              # concurrently: vLLM batches them and its prefix cache shares the state
            futs = [self.pool.submit(self.one, w[4], w[5]) for w in work]
            toks = [self.pool.submit(self.v.tokenize, w[4]) for w in work]
            reads = [f.result() for f in futs]
            # COUNT THE SHARED HEAD ONCE: the system prompt and the record are computed once by vLLM's prefix
            # cache, so N whole prompts would bill the record N times
            shared = common_prefix_len([f.result() for f in toks])
        answers, ntok = {}, 0
        for (qid, typ, names, descs, _t, _l, thr), (lp, usage) in zip(work, reads):
            answers[qid] = format_answer(typ, names, descs, lp, self.c.temperature, self.c.noul_floor, thr)
            ntok += int(usage.get("prompt_tokens") or 0)
        ntok -= shared * (len(work) - 1)   # = the shared head + each question's tail
        used = {"tier": "fast", "passes_per_decision": 1}
        if asked and asked != "fast":
            used["requested"] = asked
            used["note"] = (f"tier {asked!r} is accepted for API compatibility but not implemented; this response "
                            f"was produced at tier 'fast' with one forward pass per decision")
        return {"model": self.c.model, "answers": answers, "probability_source": "native", "effort_used": used,
                "usage": {"input_tokens": ntok, "output_tokens": 0, "latency_ms": round((time.time() - t0) * 1000, 1)}}

    def health(self):
        c = self.c
        return {"ok": bool(self.ready), "model": c.model, "shim": SHIM_VERSION, "backend": f"vllm {self.v.base}",
                "layout": c.layout, "chat_template": c.chat, "state_compact": c.state_compact, "prefill": c.prefill,
                "max_state_tokens": c.max_state_tokens,
                "noul_floor": {"value": c.noul_floor, "source": c.noul_floor_source},
                "temperature_by_type": {t: c.temperature for t in PRIMITIVES},
                "temperature_source": c.temperature_source, "max_options": len(c.labels),
                "vllm_max_model_len": self.max_model_len}


def make_handler(shim):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, fmt, *args):
            if os.environ.get("SHIM_ACCESS_LOG", "") == "1":
                sys.stderr.write("shim %s\n" % (fmt % args))

        def _send(self, code, obj):
            payload = json.dumps(obj).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def do_GET(self):
            if self.path.startswith("/health"):
                try:
                    shim.check_backend()
                except (UpstreamError, Unprocessable) as e:
                    return self._send(503, {**shim.health(), "error": str(e)})
                return self._send(200, shim.health())
            if self.path.startswith("/v1/models"):
                return self._send(200, {"object": "list", "data": [{"id": shim.c.model, "readout": "native label "
                                                                    "log-probabilities over vLLM"}]})
            self._send(404, {"error": "not found"})

        def do_POST(self):
            if not self.path.startswith(("/v1/systemone", "/api/alpha/decisions")):
                return self._send(404, {"error": "not found"})
            try:
                n = int(self.headers.get("Content-Length") or 0)
                body = json.loads(self.rfile.read(n) or b"null")
            except (ValueError, OSError) as e:
                return self._send(422, {"detail": f"request body is not JSON: {e}"})
            try:
                return self._send(200, shim.decide(body))
            except BadRequest as e:
                return self._send(400, {"detail": str(e)})
            except Unprocessable as e:
                return self._send(422, {"detail": str(e)})
            except UpstreamError as e:
                return self._send(e.status, {"detail": str(e)})
            except Exception as e:   # one item's failure is a 422, never a 500
                import traceback
                traceback.print_exc()
                return self._send(422, {"detail": f"{type(e).__name__}: {str(e)[:300]}"})

    return Handler


class Server(ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 512      # the standard library's listen backlog of 5 resets concurrent clients


def main(argv=None):
    here = os.path.dirname(os.path.abspath(__file__))
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--config", default=os.environ.get("SHIM_CONFIG", os.path.join(here, "serve_config.json")))
    ap.add_argument("--vllm", default=os.environ.get("SHIM_VLLM", "http://127.0.0.1:8000"), help="vLLM base URL")
    ap.add_argument("--model", default=os.environ.get("SHIM_MODEL"),
                    help="vLLM's --served-model-name (default: serve_config.json's `model`)")
    ap.add_argument("--host", default=os.environ.get("SHIM_HOST", "127.0.0.1"))
    ap.add_argument("--port", type=int, default=int(os.environ.get("SHIM_PORT", "8741")))
    ap.add_argument("--min-context", type=int, default=int(os.environ.get("SHIM_MIN_CONTEXT", "4096")))
    a = ap.parse_args(argv)
    cfg = json.load(open(a.config))
    if a.model:
        cfg["model"] = a.model
    contract = Contract(cfg)
    shim = Shim(contract, VLLM(a.vllm, contract.model), a.min_context)
    srv = Server((a.host, a.port), make_handler(shim))
    print(f"shim on {a.host}:{a.port} -> {a.vllm} (model {contract.model}, T {contract.temperature}, "
          f"yes/no floor {contract.noul_floor}, {len(contract.labels)} labels)", flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()
