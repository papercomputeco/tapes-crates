# python3 -I scripts/hermes-gateway.test.py
#
# Drives the hermes capture plugin the way hermes's plugin manager does — load
# the module, call register(ctx), fire the subagent_start hook, run the
# llm_request middleware over a request kwargs dict — with no hermes install
# and no network. The Rust tests pin the spellings in the asset; this proves the
# behaviour behind them.
import importlib.util
import os
import pathlib
import sys
import unittest

ASSET = pathlib.Path(__file__).resolve().parent.parent / "crates/tapes-harnesses/assets/hermes/__init__.py"
STATE_KEY = "_tapes_gateway_hermes_state"
KEYS = ("TAPES_GATEWAY_URL", "TAPES_GATEWAY_NONCE")
GATEWAY = "http://127.0.0.1:1234/launches/l1/openai/v1"


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


class HermesGatewayPlugin(unittest.TestCase):
    def setUp(self):
        self.saved = {key: os.environ.get(key) for key in KEYS}
        for key in KEYS:
            os.environ.pop(key, None)
        sys.modules.pop(STATE_KEY, None)

    def tearDown(self):
        for key, value in self.saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        sys.modules.pop(STATE_KEY, None)

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

    def test_registers_one_hook_and_one_request_middleware(self):
        _, ctx = self.launch()
        self.assertEqual(list(ctx.hooks), ["subagent_start"])
        self.assertEqual(list(ctx.middleware), ["llm_request"])

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


if __name__ == "__main__":
    unittest.main(verbosity=2)
