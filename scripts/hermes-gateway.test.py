# python3 -I scripts/hermes-gateway.test.py
#
# Drives the hermes capture plugin the way hermes's plugin manager does — load
# the module, call register(ctx), fire the subagent_start hook, run the
# llm_request middleware over a request kwargs dict — with no hermes install
# and no network. The Rust tests pin the spellings in the asset; this proves the
# behaviour behind them.
import asyncio
import base64
import importlib.util
import json
import logging
import os
import pathlib
import sys
import threading
import types
import unittest

ASSET = pathlib.Path(__file__).resolve().parent.parent / "crates/tapes-harnesses/assets/hermes/__init__.py"
STATE_KEY = "_tapes_gateway_hermes_state"
KEYS = ("TAPES_GATEWAY_URL", "TAPES_GATEWAY_NONCE", "TAPES_ALLOW_UNCAPTURED")
GATEWAY = "http://127.0.0.1:1234/launches/l1/openai/v1"


# A stand-in for hermes 0.19.0's agent/auxiliary_client.py, reduced to the shape
# the plugin depends on: call_llm / async_call_llm whose `task` is a local, and
# which build their create() kwargs through the module-global _build_call_kwargs
# (looked up at call time, as real hermes does — including on retry paths).
FAKE_AUX_SOURCE = """
def _build_call_kwargs(provider, model, messages, temperature=None, max_tokens=None,
                       tools=None, timeout=30.0, extra_body=None, reasoning_config=None,
                       base_url=None):
    return {"model": model, "messages": messages, "timeout": timeout}

def _retry(base_url):
    return _build_call_kwargs("custom", "m", [], base_url=base_url)

def call_llm(task=None, *, base_url=None, messages=(), retry=False):
    if retry:
        return _retry(base_url)
    return _build_call_kwargs("custom", "m", list(messages), base_url=base_url)

async def async_call_llm(task=None, *, base_url=None, messages=()):
    return _build_call_kwargs("custom", "m", list(messages), base_url=base_url)
"""


def install_fake_aux(source=FAKE_AUX_SOURCE):
    package = types.ModuleType("agent")
    package.__path__ = []
    module = types.ModuleType("agent.auxiliary_client")
    exec(source, module.__dict__)
    sys.modules["agent"] = package
    sys.modules["agent.auxiliary_client"] = module
    return module


def decode_metadata(value):
    return json.loads(base64.urlsafe_b64decode(value + "=" * (-len(value) % 4)))


def load():
    spec = importlib.util.spec_from_file_location("hermes_plugins.tapes_gateway", ASSET)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FakeContext:
    def __init__(self):
        self.hooks = {}
        self.middleware = {}

    def register_hook(self, name, callback):
        self.hooks.setdefault(name, []).append(callback)

    def register_middleware(self, kind, callback):
        self.middleware.setdefault(kind, []).append(callback)


class PluginCase(unittest.TestCase):
    def setUp(self):
        self.saved = {key: os.environ.get(key) for key in KEYS}
        for key in KEYS:
            os.environ.pop(key, None)
        sys.modules.pop(STATE_KEY, None)
        sys.modules.pop("agent", None)
        sys.modules.pop("agent.auxiliary_client", None)

    def tearDown(self):
        for key, value in self.saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        sys.modules.pop(STATE_KEY, None)
        sys.modules.pop("agent", None)
        sys.modules.pop("agent.auxiliary_client", None)

    def launch(self, nonce="launch-nonce"):
        os.environ["TAPES_GATEWAY_URL"] = GATEWAY
        if nonce is not None:
            os.environ["TAPES_GATEWAY_NONCE"] = nonce
        plugin = load()
        ctx = FakeContext()
        plugin.register(ctx)
        return plugin, ctx

    def request(self, ctx, session_id="sess-1", base_url=GATEWAY, request=None):
        (middleware,) = ctx.middleware["llm_request"]
        if request is None:
            request = {"model": "m", "messages": [], "extra_headers": {"anthropic-beta": "x"}}
        return middleware(
            request=request,
            original_request=dict(request),
            session_id=session_id,
            base_url=base_url,
            task_id="t",
            turn_id="turn",
            api_request_id="r",
            platform="cli",
            model="m",
            provider="custom",
            api_mode="chat_completions",
            api_call_count=1,
            telemetry_schema_version="hermes.observer.v1",
            middleware_schema_version="hermes.middleware.v1",
        )



class HermesGatewayPlugin(PluginCase):
    def test_registers_one_hook_and_request_and_execution_middleware(self):
        _, ctx = self.launch()
        self.assertEqual(list(ctx.hooks), ["subagent_start"])
        self.assertEqual(list(ctx.middleware), ["llm_request", "llm_execution"])

    def test_stamps_the_envelope_and_the_nonce_and_keeps_existing_headers(self):
        _, ctx = self.launch()
        result = self.request(ctx)
        headers = result["request"]["extra_headers"]
        self.assertEqual(headers["x-tapes-harness-id"], "hermes")
        self.assertEqual(headers["x-tapes-harness-session-id"], "sess-1")
        self.assertEqual(headers["x-tapes-gateway-nonce"], "launch-nonce")
        self.assertEqual(headers["anthropic-beta"], "x")
        self.assertIn("x-tapes-cwd", headers)
        self.assertNotIn("x-tapes-parent-harness-session-id", headers)
        self.assertNotIn("x-tapes-parent-harness-id", headers)

    def test_the_nonce_is_deleted_from_the_environment_at_load(self):
        self.launch()
        self.assertNotIn("TAPES_GATEWAY_NONCE", os.environ)

    def test_a_reload_keeps_the_nonce_it_already_took(self):
        _, _ = self.launch()
        # hermes's forced rediscovery re-executes the module; the variable is gone.
        plugin = load()
        ctx = FakeContext()
        plugin.register(ctx)
        headers = self.request(ctx)["request"]["extra_headers"]
        self.assertEqual(headers["x-tapes-gateway-nonce"], "launch-nonce")

    def test_no_nonce_means_no_echo_but_still_an_envelope(self):
        _, ctx = self.launch(nonce=None)
        headers = self.request(ctx)["request"]["extra_headers"]
        self.assertNotIn("x-tapes-gateway-nonce", headers)
        self.assertEqual(headers["x-tapes-harness-id"], "hermes")

    def test_a_delegated_subagent_carries_its_parent(self):
        _, ctx = self.launch()
        (hook,) = ctx.hooks["subagent_start"]
        hook(parent_session_id="sess-1", child_session_id="sess-child", child_goal="g")
        headers = self.request(ctx, session_id="sess-child")["request"]["extra_headers"]
        self.assertEqual(headers["x-tapes-harness-session-id"], "sess-child")
        self.assertEqual(headers["x-tapes-parent-harness-session-id"], "sess-1")
        self.assertEqual(headers["x-tapes-parent-harness-id"], "hermes")
        # And the parent's own requests stay parentless.
        parent = self.request(ctx, session_id="sess-1")["request"]["extra_headers"]
        self.assertNotIn("x-tapes-parent-harness-session-id", parent)

    def test_inert_without_a_gateway(self):
        os.environ["TAPES_GATEWAY_NONCE"] = "n"
        plugin = load()
        ctx = FakeContext()
        plugin.register(ctx)
        self.assertIsNone(self.request(ctx))
        # Inert, but the secret is still not left in the environment.
        self.assertNotIn("TAPES_GATEWAY_NONCE", os.environ)

    def test_nothing_is_stamped_toward_a_real_upstream(self):
        _, ctx = self.launch()
        for base_url in [
            "https://api.openai.com/v1",
            "http://127.0.0.1:1235/launches/l1/openai/v1",  # other port
            "http://127.0.0.1.attacker.invalid:1234/launches/l1/openai/v1",
            "http://127.0.0.1:1234/launches/l1/openai/v1-elsewhere",
            "https://127.0.0.1:1234/launches/l1/openai/v1",  # other scheme
            "",
            None,
        ]:
            self.assertIsNone(self.request(ctx, base_url=base_url), base_url)

    def test_a_request_beneath_the_gateway_mount_is_stamped(self):
        _, ctx = self.launch()
        self.assertIsNotNone(self.request(ctx, base_url=GATEWAY + "/"))
        self.assertIsNotNone(self.request(ctx, base_url=GATEWAY + "/sub"))

    def test_no_session_means_no_envelope(self):
        _, ctx = self.launch()
        self.assertIsNone(self.request(ctx, session_id=""))
        self.assertIsNone(self.request(ctx, session_id="bad\nid"))

    def test_the_cwd_encoding_matches_the_rust_producer(self):
        plugin, _ = self.launch()
        headers = plugin.envelope_headers("s", cwd='/tmp/a b/%/"/\\/ü')
        self.assertEqual(headers["x-tapes-cwd"], "/tmp/a%20b/%25/%22/%5C/%C3%BC")

    def test_an_existing_envelope_spelling_is_replaced_not_duplicated(self):
        _, ctx = self.launch()
        request = {"extra_headers": {"X-Tapes-Harness-Id": "unknown"}}
        headers = self.request(ctx, request=request)["request"]["extra_headers"]
        self.assertNotIn("X-Tapes-Harness-Id", headers)
        self.assertEqual(headers["x-tapes-harness-id"], "hermes")


class Refusal(PluginCase):
    """While capture is active, a main-loop call bound anywhere but the proxy
    fails before it is sent, through execution middleware."""

    OFF_GATEWAY = [
        "https://api.openai.com/v1",
        "http://127.0.0.1:1235/launches/l1/openai/v1",  # other port
        "http://127.0.0.1.attacker.invalid:1234/launches/l1/openai/v1",  # gateway host as a prefix
        "http://evil127.0.0.1:1234/launches/l1/openai/v1",  # gateway host as a suffix
        "http://127.0.0.1:1234/launches/l1/openai/v1-elsewhere",  # path lookalike
        "http://127.0.0.1:1234/launches/l2/openai/v1",  # other path
        "https://127.0.0.1:1234/launches/l1/openai/v1",  # other scheme
        "",
        None,
    ]

    def execute(self, ctx, base_url=GATEWAY):
        """Run one provider call through the plugin's execution middleware the
        way hermes's chain does: ``next_call`` runs the provider call on the
        payload it is given (or the original), and an error raised downstream of
        it reaches the caller."""
        (middleware,) = ctx.middleware["llm_execution"]
        request = {"model": "m", "messages": []}
        sent = []

        def provider_call(kwargs):
            sent.append(dict(**kwargs))
            return "response"

        def next_call(payload=None):
            return provider_call(request if payload is None else payload)

        result = middleware(request=request, next_call=next_call, base_url=base_url, session_id="s")
        return result, sent

    def test_an_off_gateway_main_call_is_refused_with_one_plain_line(self):
        plugin, ctx = self.launch()
        for base_url in self.OFF_GATEWAY:
            with self.assertRaises(plugin.CaptureRefused, msg=base_url) as raised:
                self.execute(ctx, base_url=base_url)
            message = str(raised.exception)
            self.assertNotIn("\n", message)
            self.assertTrue(message.startswith("This model provider bypasses capture ("), message)
            self.assertIn("TAPES_ALLOW_UNCAPTURED=1", message)

    def test_the_refusal_names_the_host_and_nothing_else(self):
        plugin, _ = self.launch()
        message = plugin.refusal_message("https://user:sk-secret@llm.example:8443/v1/chat?key=sk-other")
        self.assertIn("(llm.example:8443)", message)
        for leaked in ("sk-secret", "sk-other", "user", "/v1", "?key"):
            self.assertNotIn(leaked, message)
        self.assertIn("(no endpoint)", plugin.refusal_message(None))

    def test_nothing_is_sent_for_a_refused_call(self):
        plugin, ctx = self.launch()
        (middleware,) = ctx.middleware["llm_execution"]
        sent = []
        def next_call(payload=None):
            # Any way a provider call could read its kwargs fails.
            for read in (lambda: dict(**payload), lambda: payload.get("model"), lambda: list(payload),
                         lambda: payload["model"], lambda: dict(payload), lambda: __import__("copy").deepcopy(payload)):
                with self.assertRaises(plugin.CaptureRefused):
                    read()
            sent.append(payload)
            raise plugin.CaptureRefused("downstream")
        with self.assertRaises(plugin.CaptureRefused):
            middleware(request={"model": "m"}, next_call=next_call, base_url="https://api.openai.com/v1")
        self.assertEqual(len(sent), 1)

    def test_an_on_gateway_call_is_unaffected(self):
        _, ctx = self.launch()
        for base_url in (GATEWAY, GATEWAY + "/", GATEWAY + "/sub"):
            result, sent = self.execute(ctx, base_url=base_url)
            self.assertEqual(result, "response")
            self.assertEqual(sent, [{"model": "m", "messages": []}])

    def test_opting_out_passes_calls_through_untouched_and_is_read_at_call_time(self):
        plugin, ctx = self.launch()
        os.environ["TAPES_ALLOW_UNCAPTURED"] = "1"
        result, sent = self.execute(ctx, base_url="https://api.openai.com/v1")
        self.assertEqual((result, sent), ("response", [{"model": "m", "messages": []}]))
        for value in ("true", "yes", "0", ""):
            os.environ["TAPES_ALLOW_UNCAPTURED"] = value
            with self.assertRaises(plugin.CaptureRefused, msg=value):
                self.execute(ctx, base_url="https://api.openai.com/v1")

    def test_an_opted_out_call_to_another_host_carries_no_envelope_and_no_nonce(self):
        _, ctx = self.launch()
        os.environ["TAPES_ALLOW_UNCAPTURED"] = "1"
        (middleware,) = ctx.middleware["llm_execution"]
        stamped = {
            "model": "m",
            "messages": [],
            "extra_headers": {
                "X-Tapes-Harness-Id": "hermes",
                "x-tapes-harness-session-id": "s",
                "x-tapes-gateway-nonce": "launch-nonce",
                "anthropic-beta": "x",
            },
        }
        sent = []

        def next_call(payload=None):
            sent.append(stamped if payload is None else payload)
            return "response"

        self.assertEqual(
            middleware(request=stamped, next_call=next_call, base_url="https://api.openai.com/v1"),
            "response",
        )
        self.assertEqual(sent[0]["extra_headers"], {"anthropic-beta": "x"})
        self.assertEqual(sent[0]["model"], "m")
        # The request it was handed is not mutated in place.
        self.assertIn("x-tapes-gateway-nonce", stamped["extra_headers"])
        # On the gateway the same headers are the point, and stay.
        sent.clear()
        middleware(request=stamped, next_call=next_call, base_url=GATEWAY)
        self.assertIs(sent[0], stamped)

    def test_inert_without_a_gateway(self):
        plugin = load()
        ctx = FakeContext()
        plugin.register(ctx)
        result, sent = self.execute(ctx, base_url="https://api.openai.com/v1")
        self.assertEqual(result, "response")
        self.assertEqual(len(sent), 1)
        aux = install_fake_aux()
        sys.modules.pop(STATE_KEY, None)
        plugin = load()
        plugin.register(FakeContext())
        self.assertNotIn("extra_headers", aux.call_llm(task="title_generation", base_url="https://openrouter.ai/api/v1"))

    def test_a_hermes_without_execution_middleware_still_loads(self):
        class OldContext(FakeContext):
            def register_middleware(self, kind, callback):
                if kind != "llm_request":
                    raise ValueError(f"unknown middleware kind {kind}")
                super().register_middleware(kind, callback)

        os.environ["TAPES_GATEWAY_URL"] = GATEWAY
        plugin = load()
        ctx = OldContext()
        plugin.register(ctx)
        self.assertEqual(list(ctx.middleware), ["llm_request"])
        self.assertIsNotNone(self.request(ctx))


class AuxiliaryCalls(PluginCase):
    """Auxiliary calls skip request middleware; the plugin stamps them through
    hermes's shared kwargs builder instead."""

    def launch_with_aux(self, nonce="launch-nonce"):
        aux = install_fake_aux()
        plugin, ctx = self.launch(nonce=nonce)
        # A main turn names the session, as hermes's loop would before any
        # title or compression call.
        self.assertIsNotNone(self.request(ctx, session_id="sess-main"))
        return plugin, ctx, aux

    def assert_aux_envelope(self, kwargs, task, session="sess-main"):
        headers = kwargs["extra_headers"]
        self.assertEqual(headers["x-tapes-harness-id"], "hermes")
        self.assertEqual(headers["x-tapes-harness-session-id"], session)
        self.assertEqual(headers["x-tapes-gateway-nonce"], "launch-nonce")
        self.assertIn("x-tapes-cwd", headers)
        self.assertEqual(decode_metadata(headers["x-tapes-harness-metadata"]), {"aux_task": task})

    def test_title_generation_on_the_main_thread_carries_the_session(self):
        _, _, aux = self.launch_with_aux()
        self.assert_aux_envelope(aux.call_llm(task="title_generation", base_url=GATEWAY), "title_generation")

    def test_title_generation_on_a_background_thread_carries_the_session(self):
        _, _, aux = self.launch_with_aux()
        out = {}
        # A plain thread does not inherit the main context's variables, so this
        # exercises the process-wide fallback.
        worker = threading.Thread(target=lambda: out.update(kw=aux.call_llm(task="title_generation", base_url=GATEWAY)))
        worker.start()
        worker.join()
        self.assert_aux_envelope(out["kw"], "title_generation")

    def test_retry_paths_and_async_calls_are_stamped_with_their_task(self):
        _, _, aux = self.launch_with_aux()
        self.assert_aux_envelope(aux.call_llm(task="compression", base_url=GATEWAY, retry=True), "compression")
        kwargs = asyncio.run(aux.async_call_llm(task="vision", base_url=GATEWAY))
        self.assert_aux_envelope(kwargs, "vision")

    def test_the_context_session_wins_over_the_process_fallback(self):
        _, ctx, aux = self.launch_with_aux()
        import contextvars

        def in_other_session():
            self.request(ctx, session_id="sess-other")
            return aux.call_llm(task="compression", base_url=GATEWAY)

        kwargs = contextvars.copy_context().run(in_other_session)
        self.assertEqual(kwargs["extra_headers"]["x-tapes-harness-session-id"], "sess-other")

    def test_a_delegated_subagents_aux_call_carries_its_parent(self):
        _, ctx, aux = self.launch_with_aux()
        (hook,) = ctx.hooks["subagent_start"]
        hook(parent_session_id="sess-main", child_session_id="sess-child")
        self.request(ctx, session_id="sess-child")
        headers = aux.call_llm(task="web_extract", base_url=GATEWAY)["extra_headers"]
        self.assertEqual(headers["x-tapes-parent-harness-session-id"], "sess-main")
        self.assertEqual(headers["x-tapes-parent-harness-id"], "hermes")

    def test_an_off_gateway_aux_call_is_refused(self):
        plugin, _, aux = self.launch_with_aux()
        for base_url in ["https://openrouter.ai/api/v1", "http://127.0.0.1:1235/v1", None]:
            with self.assertRaises(plugin.CaptureRefused, msg=base_url) as raised:
                aux.call_llm(task="title_generation", base_url=base_url)
            self.assertIn("bypasses capture", str(raised.exception))
        # The retry path and async calls go through the same builder.
        with self.assertRaises(plugin.CaptureRefused):
            aux.call_llm(task="compression", base_url="https://openrouter.ai/api/v1", retry=True)
        with self.assertRaises(plugin.CaptureRefused):
            asyncio.run(aux.async_call_llm(task="vision", base_url="https://openrouter.ai/api/v1"))

    def test_an_off_gateway_aux_call_passes_untouched_when_opted_out(self):
        _, _, aux = self.launch_with_aux()
        os.environ["TAPES_ALLOW_UNCAPTURED"] = "1"
        self.assertNotIn("extra_headers", aux.call_llm(task="title_generation", base_url="https://openrouter.ai/api/v1"))

    def test_no_aux_stamp_before_any_session_or_without_a_gateway(self):
        aux = install_fake_aux()
        _, _ = self.launch()
        self.assertNotIn("extra_headers", aux.call_llm(task="title_generation", base_url=GATEWAY))
        os.environ.pop("TAPES_GATEWAY_URL")
        self.assertNotIn("extra_headers", aux.call_llm(task="title_generation", base_url=GATEWAY))

    def test_a_reload_wraps_the_original_once(self):
        _, _, aux = self.launch_with_aux()
        plugin = load()
        plugin.register(FakeContext())
        self.assertTrue(aux._build_call_kwargs.__tapes_wrapped__)
        self.assertFalse(getattr(aux._build_call_kwargs.__wrapped__, "__tapes_wrapped__", False))
        self.assert_aux_envelope(aux.call_llm(task="mcp", base_url=GATEWAY), "mcp")

    def test_a_hermes_without_the_expected_shape_is_left_alone(self):
        # No auxiliary module at all: register must not raise.
        _, ctx = self.launch()
        self.assertIsNotNone(self.request(ctx))
        sys.modules.pop(STATE_KEY, None)
        # A builder without base_url: skipped, not wrapped.
        aux = install_fake_aux(FAKE_AUX_SOURCE.replace("reasoning_config=None,\n                       base_url=None", "reasoning_config=None").replace(", base_url=base_url", ""))
        plugin, _ = self.launch()
        self.assertFalse(hasattr(aux._build_call_kwargs, "__tapes_wrapped__"))
        # Missing call_llm: skipped too.
        sys.modules.pop(STATE_KEY, None)
        aux = install_fake_aux("def _build_call_kwargs(base_url=None):\n    return {}\n")
        self.launch()
        self.assertFalse(hasattr(aux._build_call_kwargs, "__tapes_wrapped__"))

    def test_an_opted_out_aux_call_to_another_host_leaves_bare(self):
        # Whatever put an envelope header on the kwargs, an uncaptured call to
        # another host does not carry it; the same call on the gateway does.
        aux = install_fake_aux(FAKE_AUX_SOURCE.replace(
            'return {"model": model, "messages": messages, "timeout": timeout}',
            'return {"model": model, "extra_headers": {"x-tapes-harness-id": "hermes", "x-tapes-gateway-nonce": "n", "anthropic-beta": "x"}}',
        ))
        self.launch()
        os.environ["TAPES_ALLOW_UNCAPTURED"] = "1"
        bare = aux.call_llm(task="title_generation", base_url="https://openrouter.ai/api/v1")
        self.assertEqual(bare["extra_headers"], {"anthropic-beta": "x"})

    def test_metadata_encoding_is_compact_base64url_without_padding(self):
        plugin, _ = self.launch()
        value = plugin._metadata_value({"aux_task": "title_generation"})
        self.assertNotIn("=", value)
        self.assertEqual(base64.urlsafe_b64decode(value + "=" * (-len(value) % 4)), b'{"aux_task":"title_generation"}')
        self.assertIsNone(plugin._metadata_value({"aux_task": "x" * 5000}))

    def test_the_header_budget_drops_metadata_then_cwd_but_keeps_identity(self):
        plugin, _ = self.launch()
        meta = plugin._metadata_value({"aux_task": "t" * 3000})
        headers = plugin.envelope_headers("s", cwd="/" + "d" * 6000)
        headers["x-tapes-harness-metadata"] = meta
        fitted = plugin.fit_budget(headers)
        self.assertLessEqual(plugin._envelope_cost(fitted), plugin.X_TAPES_TOTAL_BUDGET)
        self.assertNotIn("x-tapes-harness-metadata", fitted)
        self.assertIn("x-tapes-cwd", fitted)
        huge = plugin.fit_budget(plugin.envelope_headers("s", cwd="/" + "d" * 9000))
        self.assertNotIn("x-tapes-cwd", huge)
        self.assertEqual(huge["x-tapes-harness-session-id"], "s")
        self.assertEqual(huge["x-tapes-harness-id"], "hermes")

    def test_the_header_budget_never_splits_the_parent_pair(self):
        # A parent session id without its harness id reads as same-harness, so
        # budget pressure gives up both or neither. Sized one byte over: giving
        # up the parent harness id alone would fit and strand the session id.
        plugin, _ = self.launch()
        parent = {"x-tapes-parent-harness-session-id": "p", "x-tapes-parent-harness-id": "hermes"}
        identity = {"x-tapes-harness-id": "hermes", "x-tapes-harness-session-id": ""}
        filler = plugin.X_TAPES_TOTAL_BUDGET + 1 - plugin._envelope_cost({**identity, **parent})
        headers = {**identity, "x-tapes-harness-session-id": "s" * filler, **parent}
        self.assertEqual(plugin._envelope_cost(headers), plugin.X_TAPES_TOTAL_BUDGET + 1)
        fitted = plugin.fit_budget(headers)
        self.assertLessEqual(plugin._envelope_cost(fitted), plugin.X_TAPES_TOTAL_BUDGET)
        self.assertNotIn("x-tapes-parent-harness-session-id", fitted)
        self.assertNotIn("x-tapes-parent-harness-id", fitted)
        self.assertEqual(fitted["x-tapes-harness-session-id"], "s" * filler)

    def test_the_header_budget_keeps_a_parent_pair_that_fits(self):
        plugin, _ = self.launch()
        headers = plugin.envelope_headers("s", cwd="/" + "d" * 9000)
        headers["x-tapes-parent-harness-session-id"] = "p"
        headers["x-tapes-parent-harness-id"] = "hermes"
        fitted = plugin.fit_budget(headers)
        self.assertNotIn("x-tapes-cwd", fitted)
        self.assertEqual(fitted["x-tapes-parent-harness-session-id"], "p")
        self.assertEqual(fitted["x-tapes-parent-harness-id"], "hermes")

    def test_an_aux_task_name_is_bounded(self):
        _, _, aux = self.launch_with_aux()
        headers = aux.call_llm(task="z" * 1000, base_url=GATEWAY)["extra_headers"]
        self.assertEqual(len(decode_metadata(headers["x-tapes-harness-metadata"])["aux_task"]), 128)


if __name__ == "__main__":
    # The plugin warns once when it cannot hook auxiliary calls; most cases
    # here load it without a fake hermes on purpose.
    logging.disable(logging.WARNING)
    unittest.main(verbosity=2)
