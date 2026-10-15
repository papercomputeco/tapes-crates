"""Capture plugin for hermes-agent: stamps the X-Tapes-* envelope from inside.

hermes keeps its sessions in SQLite and publishes nothing a capture client can
index by PID, so a redirected session's turns would otherwise file under
``harness_id: unknown``. This plugin closes that gap the way the pi and
opencode extensions do: it registers LLM request middleware and adds the
envelope to every outbound model request's ``extra_headers`` — hermes's own
session id, the parent session for a delegated subagent, the working
directory, the hermes version, and the capture-nonce echo.

The same envelope reaches hermes's *auxiliary* calls — title generation,
compression summaries, vision, web extraction, session search, skills hub,
MCP sampling — which never run through request middleware. Those all build
their request kwargs through ``agent.auxiliary_client._build_call_kwargs``, so
the plugin wraps that one function (see ``_install_auxiliary_stamping``) and
adds the envelope plus ``x-tapes-harness-metadata`` ``{"aux_task": <task>}``
to the kwargs it returns. The session id comes from the session the request
middleware last saw in this execution context, falling back to the last one
seen anywhere in the process for a thread that did not inherit the context.

While capture is active it also **refuses** an outbound model call that would
leave it: a main-loop request (through ``llm_execution`` middleware) or an
auxiliary call (through the same kwargs builder) whose base URL is not on the
capture proxy fails with one line naming the host, instead of going out
uncaptured. ``TAPES_ALLOW_UNCAPTURED=1``, read at call time, lets such calls
through untouched.

It does **not** redirect. hermes reads its endpoint from ``config.yaml``
(``model.provider: custom`` plus ``model.base_url``), and middleware can
rewrite a request's kwargs but not the client's base URL, so pointing hermes
at a capture proxy is the launch recipe's job: it writes that config into an
isolated ``HERMES_HOME`` alongside this plugin. What this file decides is
whether a given request is going to that proxy at all — the envelope and the
nonce are only ever put on one that is.

The whole contract is a handful of environment variables the launching client
sets; their spellings are pinned against the Rust constants by the crate's
tests. Unset ``TAPES_GATEWAY_URL`` means "not captured" and the plugin stays
inert, which is what lets it sit installed in a home that also runs ordinary
sessions.
"""

from __future__ import annotations

import base64
import contextvars
import inspect
import json
import logging
import os
import sys
import threading
import types
import urllib.parse
from typing import Any, Dict, Optional

# Names the launching capture client sets. The whole contract between this
# asset and whoever installed it — the crate's tests pin each spelling against
# its Rust constant.
GATEWAY_URL_ENV = "TAPES_GATEWAY_URL"
GATEWAY_NONCE_ENV = "TAPES_GATEWAY_NONCE"
# The opt-out from refusing uncaptured calls, read at call time. Exactly "1"
# lets a call bound somewhere other than the proxy go out untouched (logged
# once); anything else keeps the refusal, so a typo cannot turn capture off.
ALLOW_UNCAPTURED_ENV = "TAPES_ALLOW_UNCAPTURED"
ALLOW_UNCAPTURED_ON = "1"

# The header the nonce is echoed back in. A per-launch secret: the proxy's
# peer-PID ancestry check cannot tell this process's requests apart from those
# of its own subprocesses (a terminal tool's child is a descendant too), and the
# echo is what does. The proxy validates and strips it before forwarding; it
# must never be logged or written anywhere else.
GATEWAY_NONCE_HEADER = "x-tapes-gateway-nonce"

HARNESS_ID = "hermes"

_logger = logging.getLogger(__name__)

# The envelope this plugin stamps, in the crate's lower-case wire spelling.
ENVELOPE = {
    "harness_id": "x-tapes-harness-id",
    "session_id": "x-tapes-harness-session-id",
    "version": "x-tapes-harness-version",
    "cwd": "x-tapes-cwd",
    "parent_session_id": "x-tapes-parent-harness-session-id",
    "parent_harness_id": "x-tapes-parent-harness-id",
    "metadata": "x-tapes-harness-metadata",
}

# The envelope's byte budgets, mirroring tapes-capture: the whole x-tapes-*
# set within 8 KiB (name + value bytes), the metadata blob within 4 KiB of raw
# JSON. Metadata is the first thing dropped, then the other optional fields;
# the harness id and session id always stay.
X_TAPES_TOTAL_BUDGET = 8 * 1024
X_TAPES_METADATA_RAW_CAP = 4 * 1024
# Optional fields in the order they are given up under budget pressure. The
# parent's session id and harness id are one entry: a parent session id with no
# harness id beside it reads as "same harness as this session", so the two are
# only ever given up together.
_DROP_ORDER = ("metadata", "cwd", "version", "parent")
_DROP_FIELDS = {"parent": ("parent_session_id", "parent_harness_id")}

# The longest aux task name carried in metadata. hermes's names are short
# (`title_generation`); the cap only keeps a pathological value bounded.
_AUX_TASK_CAP = 128

# Everything the envelope's working-directory encoding leaves alone: visible
# ASCII except `%`, `"` and `\`. Mirrors the Rust producer's escape set (controls,
# space, `%`, `"`, `\`, DEL, and every non-ASCII byte are encoded), so a cwd
# stamped here and one stamped by a capture client are byte-identical.
_CWD_SAFE = "".join(chr(c) for c in range(0x21, 0x7F) if chr(c) not in '%"\\')

# State that must survive a plugin reload. hermes's `discover_and_load(force=True)`
# re-executes this module from scratch; module globals would then re-read an
# environment from which the nonce has already been deleted, and lose the
# subagent lineage collected so far. A private entry in `sys.modules` is the one
# place a re-executed module can find its predecessor's values.
_STATE_KEY = "_tapes_gateway_hermes_state"
_state = sys.modules.get(_STATE_KEY)
if _state is None:
    _state = types.ModuleType(_STATE_KEY)
    # Take the launch secret out of the environment before anything else runs.
    # hermes loads plugins at startup, before the first session and therefore
    # before any tool can spawn a subprocess. Children inherit the process's
    # *current* environment, so popping the variable here means they never
    # receive it; the value survives only in this process's memory. Popped even
    # when no gateway is configured: an inert plugin must not leave the secret
    # lying in the environment either.
    _state.nonce = os.environ.get(GATEWAY_NONCE_ENV) or None
    os.environ.pop(GATEWAY_NONCE_ENV, None)
    # child session id -> parent session id, fed by the `subagent_start` hook.
    _state.parents = {}
    _state.lock = threading.Lock()
    # The session the request middleware saw last, per execution context and
    # process-wide. Auxiliary calls carry no session id of their own (hermes's
    # `main_runtime` has model, provider and endpoint, but not the session), so
    # they borrow this. The context variable is right whenever the auxiliary
    # call runs where the main turn did; the process-wide value covers a
    # background thread that did not inherit the context (title generation
    # runs on one). With one agent per process — a launched hermes — the two
    # always agree. With several concurrent sessions in one process (hermes's
    # gateway mode) a thread-spawned auxiliary call may attach to whichever
    # session made the most recent main request.
    _state.active_session = contextvars.ContextVar("tapes_hermes_active_session", default=None)
    _state.last_session = None
    _state.aux_original = None
    _state.aux_skip_logged = False
    _state.refusal_skip_logged = False
    _state.uncaptured_logged = False
    sys.modules[_STATE_KEY] = _state

# A re-executed module inherits the state but must still never leave the secret
# in the environment, whatever set it in between.
os.environ.pop(GATEWAY_NONCE_ENV, None)


def _normalize_base_url(url: str) -> str:
    trimmed = url.strip().rstrip("/")
    if trimmed.startswith("http://") or trimmed.startswith("https://"):
        return trimmed
    return f"http://{trimmed}"


def _gateway_url() -> Optional[str]:
    raw = os.environ.get(GATEWAY_URL_ENV, "").strip()
    return _normalize_base_url(raw) if raw else None


def is_gateway_address(candidate: Any, gateway_url: str) -> bool:
    """Is ``candidate`` an address on the capture proxy at ``gateway_url``?

    The gate on the nonce and the envelope, so a boundary comparison on parsed
    URLs rather than a string prefix: a prefix test would accept
    ``https://gw.example.attacker.invalid`` for a gateway at
    ``https://gw.example`` and hand that host the launch secret. Scheme, host
    and port must all match, and the path must be the gateway's mount or lie
    beneath it on a separator — ``/capture`` never matches ``/capture-elsewhere``.
    """
    if not isinstance(candidate, str) or not candidate.strip():
        return False
    try:
        url = urllib.parse.urlsplit(candidate.strip())
        gateway = urllib.parse.urlsplit(gateway_url)
        url_port = url.port
        gateway_port = gateway.port
    except ValueError:
        return False
    if url.scheme not in ("http", "https") or not url.hostname:
        return False
    default = {"http": 80, "https": 443}
    if (
        url.scheme != gateway.scheme
        or url.hostname != gateway.hostname
        or (url_port or default[url.scheme]) != (gateway_port or default[gateway.scheme])
    ):
        return False
    mount = gateway.path.rstrip("/")
    path = url.path.rstrip("/")
    return path == mount or path.startswith(f"{mount}/")


def _encode_cwd(cwd: str) -> str:
    return urllib.parse.quote(cwd, safe=_CWD_SAFE, encoding="utf-8", errors="surrogateescape")


def _hermes_version() -> Optional[str]:
    try:
        from hermes_cli import __version__

        return str(__version__) or None
    except Exception:
        return None


def _header_safe(value: Any) -> Optional[str]:
    """A value usable as a raw header, or ``None``. Absent means no evidence:
    a blank or control-bearing value is dropped rather than sent."""
    if not isinstance(value, str):
        return None
    value = value.strip()
    if not value or any(ord(c) < 0x20 or ord(c) == 0x7F or ord(c) > 0x7E for c in value):
        return None
    return value


def envelope_headers(session_id: Any, cwd: Optional[str] = None) -> Dict[str, str]:
    """The envelope for one request from ``session_id``.

    Empty when there is no usable session id: an envelope without one is
    incomplete, and a capture client would rightly refuse to believe it.
    """
    sid = _header_safe(session_id)
    if sid is None:
        return {}
    headers = {ENVELOPE["harness_id"]: HARNESS_ID, ENVELOPE["session_id"]: sid}
    version = _header_safe(_hermes_version())
    if version:
        headers[ENVELOPE["version"]] = version
    if cwd is None:
        try:
            cwd = os.getcwd()
        except OSError:
            cwd = None
    if cwd:
        headers[ENVELOPE["cwd"]] = _encode_cwd(cwd)
    with _state.lock:
        parent = _state.parents.get(sid)
    parent = _header_safe(parent)
    # Only hermes's own delegation is known here, so the parent is always a
    # hermes session. A parent from *another* harness (hermes launched inside a
    # Claude Code session) is resolved by the launching client from the
    # TAPES_LAUNCH_* lineage variables — this process is only told its own
    # launch, never the parent's session — and stamped by that client.
    if parent and parent != sid:
        headers[ENVELOPE["parent_session_id"]] = parent
        headers[ENVELOPE["parent_harness_id"]] = HARNESS_ID
    return headers


def _metadata_value(metadata: Dict[str, Any]) -> Optional[str]:
    """base64url (no padding) of compact JSON, or ``None`` over the raw cap —
    the same encoding the Rust producer uses."""
    raw = json.dumps(metadata, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    if len(raw) > X_TAPES_METADATA_RAW_CAP:
        return None
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")


def _envelope_cost(headers: Dict[str, str]) -> int:
    return sum(
        len(name) + len(value.encode("utf-8"))
        for name, value in headers.items()
        if name.startswith("x-tapes-")
    )


def fit_budget(headers: Dict[str, str]) -> Dict[str, str]:
    """Drop optional envelope fields, metadata first, until the set fits."""
    headers = dict(headers)
    for field in _DROP_ORDER:
        if _envelope_cost(headers) <= X_TAPES_TOTAL_BUDGET:
            break
        for name in _DROP_FIELDS.get(field, (field,)):
            headers.pop(ENVELOPE[name], None)
    return headers


def _merge_headers(existing: Any, headers: Dict[str, str]) -> Dict[str, Any]:
    """Merge ``headers`` into an ``extra_headers`` value, replacing any
    envelope header already present under another casing so the request
    carries exactly one spelling of each."""
    merged = dict(existing) if isinstance(existing, dict) else {}
    stamped = {name.lower() for name in headers}
    merged = {k: v for k, v in merged.items() if str(k).lower() not in stamped}
    merged.update(headers)
    return merged


def _remember_session(session_id: Any) -> None:
    sid = _header_safe(session_id)
    if sid is None:
        return
    _state.active_session.set(sid)
    with _state.lock:
        _state.last_session = sid


def current_session() -> Optional[str]:
    """The session an auxiliary call belongs to: this context's, else the
    process's most recent (see the state notes above for the caveat)."""
    sid = _state.active_session.get()
    if sid:
        return sid
    with _state.lock:
        return _state.last_session


def on_subagent_start(
    parent_session_id: Any = None, child_session_id: Any = None, **_: Any
) -> None:
    """Record a delegated subagent's parent session, so its requests carry it."""
    child = _header_safe(child_session_id)
    parent = _header_safe(parent_session_id)
    if child and parent and child != parent:
        with _state.lock:
            _state.parents[child] = parent


def llm_request_middleware(
    request: Any = None, session_id: Any = None, base_url: Any = None, **_: Any
) -> Optional[Dict[str, Any]]:
    """Stamp the envelope and the nonce echo onto a request bound for the proxy.

    Returns ``None`` — leave the request untouched — when nothing is being
    captured, when the request is not going to the capture proxy (a fallback
    provider, an auxiliary model on its own endpoint), or when there is no
    session to attribute it to. The nonce is a secret shared with the proxy
    alone, so it is never put on a request headed anywhere else.
    """
    gateway = _gateway_url()
    if gateway is None or not isinstance(request, dict):
        return None
    if not is_gateway_address(base_url, gateway):
        return None
    headers = fit_budget(envelope_headers(session_id))
    if not headers:
        return None
    _remember_session(session_id)
    if _state.nonce:
        headers[GATEWAY_NONCE_HEADER] = _state.nonce
    request = dict(request)
    request["extra_headers"] = _merge_headers(request.get("extra_headers"), headers)
    return {"request": request, "source": "tapes-gateway", "reason": "capture envelope"}


# --- refusing calls that would leave capture ----------------------------------
#
# Why `llm_execution` and not `llm_request`: hermes isolates request middleware —
# `invoke_middleware` catches whatever a callback raises, logs it, and carries on
# with the request unchanged — so raising there refuses nothing. Returning a
# poisoned replacement request does not work either: the result is deep-copied
# (falling back to `dict(...)`) inside `apply_llm_request_middleware`, and an
# error there is caught by the agent loop, which then sends the *original*
# kwargs. Execution middleware is isolated the same way for its own exceptions
# (the chain logs and calls the next stage), but an exception raised *downstream*
# of `next_call` is re-raised to the agent loop as the provider call's error. So
# the refusal is handed downstream: `next_call` gets a request whose every read
# raises `CaptureRefused`, and the provider call fails on its first look at its
# kwargs — before a client is built or a byte is sent — with the refusal as its
# error. The agent loop treats it as a failed API call: it may retry, and every
# retry is refused the same way, then reports it.
#
# Auxiliary calls need none of this: the plugin's own wrapper around
# `_build_call_kwargs` sits on the call path, so it raises directly.


class CaptureRefused(RuntimeError):
    """A model call refused because it would bypass the capture proxy."""


def _allow_uncaptured() -> bool:
    return os.environ.get(ALLOW_UNCAPTURED_ENV, "") == ALLOW_UNCAPTURED_ON


def _display_host(base_url: Any) -> str:
    """The host (and port) a call was bound for — never userinfo, path or query."""
    if not isinstance(base_url, str) or not base_url.strip():
        return "no endpoint"
    try:
        url = urllib.parse.urlsplit(base_url.strip())
        host = url.hostname
        port = url.port
    except ValueError:
        return "unparseable endpoint"
    if not host:
        return "unparseable endpoint"
    host = _header_safe(host) or "unparseable endpoint"
    return f"{host}:{port}" if port else host


def refusal_message(base_url: Any) -> str:
    return (
        f"This model provider bypasses capture ({_display_host(base_url)}). "
        "Use a model routed through the tapes gateway, "
        f"or set {ALLOW_UNCAPTURED_ENV}=1 to allow uncaptured calls."
    )


def refuses(base_url: Any) -> bool:
    """Whether a call bound for ``base_url`` must be refused: capture is active,
    the address is not on the proxy, and the user has not opted out. An opted-out
    call is let through, and said so once."""
    gateway = _gateway_url()
    if gateway is None or is_gateway_address(base_url, gateway):
        return False
    if _allow_uncaptured():
        if not _state.uncaptured_logged:
            _state.uncaptured_logged = True
            _logger.warning(
                "tapes-gateway: %s=1; a model call to %s is not captured",
                ALLOW_UNCAPTURED_ENV,
                _display_host(base_url),
            )
        return False
    return True


class _RefusedRequest(dict):
    """An empty request mapping whose every read raises the refusal.

    Handed downstream in place of a refused call's kwargs: the first thing the
    provider call does with them — unpack, read, copy, iterate — fails before
    any client is built. It is empty underneath, so even a reader that bypassed
    every override would build a call with no model and no messages."""

    def __init__(self, error: CaptureRefused) -> None:
        super().__init__()
        self._error = error

    def _refuse(self, *_: Any, **__: Any) -> Any:
        raise self._error

    __getitem__ = __iter__ = __len__ = __contains__ = _refuse
    get = keys = items = values = copy = pop = setdefault = update = _refuse
    __copy__ = __deepcopy__ = __reduce__ = __reduce_ex__ = _refuse

    def __bool__(self) -> bool:
        return True


def llm_execution_middleware(
    request: Any = None, next_call: Any = None, base_url: Any = None, **_: Any
) -> Any:
    """Run the provider call — or, for a call that would bypass the capture
    proxy, fail it before it is sent (see the notes above)."""
    if not callable(next_call):
        return None
    if refuses(base_url):
        return next_call(_RefusedRequest(CaptureRefused(refusal_message(base_url))))
    return next_call()


# --- auxiliary calls ---------------------------------------------------------
#
# Why `_build_call_kwargs` and not `call_llm`: the callers bind `call_llm` at
# import (`from agent.auxiliary_client import call_llm` in the title generator
# and the compressor), so replacing the module attribute after they loaded
# reaches none of them. `_build_call_kwargs` is looked up as a module global at
# every call, and it is the one function every auxiliary path — sync, async,
# streaming, the max-tokens and payment retries, the provider fallback chain —
# builds its `chat.completions.create(**kwargs)` from. It is also handed the
# *client's* resolved base URL, which is exactly what the gateway check needs.
# What it is not handed is the task name; that is read from the enclosing
# `call_llm` / `async_call_llm` frame's `task` local, and an aux call whose frame
# cannot be found is still stamped, as task `auxiliary`.

_AUX_MODULE = "agent.auxiliary_client"
_AUX_ENTRY_POINTS = ("call_llm", "async_call_llm")
_AUX_FRAME_DEPTH = 12


def _aux_task_from_stack() -> str:
    frame = sys._getframe(2)
    for _ in range(_AUX_FRAME_DEPTH):
        if frame is None:
            break
        if (
            frame.f_code.co_name in _AUX_ENTRY_POINTS
            and frame.f_globals.get("__name__") == _AUX_MODULE
        ):
            task = frame.f_locals.get("task")
            if isinstance(task, str) and task.strip():
                return task.strip()[:_AUX_TASK_CAP]
            break
        frame = frame.f_back
    return "auxiliary"


def auxiliary_headers(base_url: Any, task: str) -> Dict[str, str]:
    """The envelope for one auxiliary call, or ``{}`` when it must go bare:
    nothing is being captured, the call is not bound for the capture proxy, or
    no main request has named a session yet."""
    gateway = _gateway_url()
    if gateway is None or not is_gateway_address(base_url, gateway):
        return {}
    headers = envelope_headers(current_session())
    if not headers:
        return {}
    metadata = _metadata_value({"aux_task": task})
    if metadata:
        headers[ENVELOPE["metadata"]] = metadata
    headers = fit_budget(headers)
    if _state.nonce:
        headers[GATEWAY_NONCE_HEADER] = _state.nonce
    return headers


def _wrap_build_call_kwargs(original: Any, signature: inspect.Signature) -> Any:
    def build_call_kwargs(*args: Any, **kwargs: Any) -> Any:
        try:
            base_url = signature.bind_partial(*args, **kwargs).arguments.get("base_url")
        except TypeError:
            # hermes raises the same binding error itself, below.
            pass
        else:
            # The refusal is raised from the caller's own kwargs build, so the
            # auxiliary call fails here and is never sent.
            if refuses(base_url):
                raise CaptureRefused(refusal_message(base_url))
        result = original(*args, **kwargs)
        try:
            if isinstance(result, dict):
                headers = auxiliary_headers(base_url, _aux_task_from_stack())
                if headers:
                    result["extra_headers"] = _merge_headers(result.get("extra_headers"), headers)
        except Exception:
            # Attribution is best-effort; the call itself must never fail here.
            pass
        return result

    build_call_kwargs.__tapes_wrapped__ = True
    build_call_kwargs.__wrapped__ = original
    return build_call_kwargs


def _skip_auxiliary(reason: str) -> None:
    if not _state.aux_skip_logged:
        _state.aux_skip_logged = True
        _logger.warning(
            "tapes-gateway: auxiliary calls will not carry the capture envelope (%s); "
            "they still carry the static harness id from config.yaml",
            reason,
        )


def _install_auxiliary_stamping() -> bool:
    """Wrap hermes's auxiliary kwargs builder, once, if this hermes has the
    shape it was written against. Returns whether stamping is installed."""
    try:
        import importlib

        aux = importlib.import_module(_AUX_MODULE)
    except Exception as exc:
        _skip_auxiliary(f"{_AUX_MODULE} did not import: {exc}")
        return False
    if not all(callable(getattr(aux, name, None)) for name in _AUX_ENTRY_POINTS):
        _skip_auxiliary("call_llm/async_call_llm not found")
        return False
    current = getattr(aux, "_build_call_kwargs", None)
    # A reload re-wraps the original rather than stacking a second wrapper.
    original = _state.aux_original or current
    if not callable(original):
        _skip_auxiliary("_build_call_kwargs not found")
        return False
    try:
        signature = inspect.signature(original)
    except (TypeError, ValueError):
        _skip_auxiliary("_build_call_kwargs has no inspectable signature")
        return False
    if "base_url" not in signature.parameters:
        _skip_auxiliary("_build_call_kwargs takes no base_url")
        return False
    _state.aux_original = original
    aux._build_call_kwargs = _wrap_build_call_kwargs(original, signature)
    return True


def register(ctx: Any) -> None:
    """hermes plugin entry point."""
    ctx.register_hook("subagent_start", on_subagent_start)
    ctx.register_middleware("llm_request", llm_request_middleware)
    try:
        ctx.register_middleware("llm_execution", llm_execution_middleware)
    except Exception as exc:
        # A hermes without execution middleware still gets the envelope; it
        # just cannot have its main-loop calls refused.
        if not _state.refusal_skip_logged:
            _state.refusal_skip_logged = True
            _logger.warning(
                "tapes-gateway: main-loop calls that bypass capture cannot be refused (%s)",
                exc,
            )
    _install_auxiliary_stamping()
