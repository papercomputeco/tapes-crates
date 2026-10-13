"""Capture plugin for hermes-agent: stamps the X-Tapes-* envelope from inside.

hermes keeps its sessions in SQLite and publishes nothing a capture client can
index by PID, so a redirected session's turns would otherwise file under
``harness_id: unknown``. This plugin closes that gap the way the pi and
opencode extensions do: it registers LLM request middleware and adds the
envelope to every outbound model request's ``extra_headers`` — hermes's own
session id, the parent session for a delegated subagent, the working
directory, the hermes version, and the capture-nonce echo.

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

# The header the nonce is echoed back in. A per-launch secret: the proxy's
# peer-PID ancestry check cannot tell this process's requests apart from those
# of its own subprocesses (a terminal tool's child is a descendant too), and the
# echo is what does. The proxy validates and strips it before forwarding; it
# must never be logged or written anywhere else.
GATEWAY_NONCE_HEADER = "x-tapes-gateway-nonce"

HARNESS_ID = "hermes"

# The envelope this plugin stamps, in the crate's lower-case wire spelling.
ENVELOPE = {
    "harness_id": "x-tapes-harness-id",
    "session_id": "x-tapes-harness-session-id",
    "version": "x-tapes-harness-version",
    "cwd": "x-tapes-cwd",
    "parent_session_id": "x-tapes-parent-harness-session-id",
    "parent_harness_id": "x-tapes-parent-harness-id",
}

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
    headers = envelope_headers(session_id)
    if not headers:
        return None
    if _state.nonce:
        headers[GATEWAY_NONCE_HEADER] = _state.nonce
    existing = request.get("extra_headers")
    merged = dict(existing) if isinstance(existing, dict) else {}
    # Drop any envelope header already present under another casing, so the
    # request carries exactly one spelling of each and it is this one.
    stamped = {name.lower() for name in headers}
    merged = {k: v for k, v in merged.items() if str(k).lower() not in stamped}
    merged.update(headers)
    request = dict(request)
    request["extra_headers"] = merged
    return {"request": request, "source": "tapes-gateway", "reason": "capture envelope"}


def register(ctx: Any) -> None:
    """hermes plugin entry point."""
    ctx.register_hook("subagent_start", on_subagent_start)
    ctx.register_middleware("llm_request", llm_request_middleware)
