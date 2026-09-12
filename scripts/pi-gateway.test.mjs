// Node 24+: node --experimental-strip-types --test scripts/pi-gateway.test.mjs
import { test, afterEach } from "node:test";
import assert from "node:assert/strict";
import gateway from "../crates/tapes-harnesses/assets/pi/tapes-gateway.ts";

const keys = ["TAPES_GATEWAY_URL", "TAPES_GATEWAY_NONCE", "TAPES_GATEWAY_PROVIDER_CONFIG", "TAPES_GATEWAY_PROVIDER_ROUTES", "TAPES_GATEWAY_SCHEMA"];
const original = Object.fromEntries(keys.map(key => [key, process.env[key]]));
afterEach(() => {
  for (const key of keys) {
    if (original[key] === undefined) delete process.env[key];
    else process.env[key] = original[key];
  }
});
function setup(provider) {
  for (const key of keys) delete process.env[key];
  process.env.TAPES_GATEWAY_URL = "http://127.0.0.1:1234/launches/pilot/openai";
  process.env.TAPES_GATEWAY_NONCE = "nonce";
  if (provider !== undefined) process.env.TAPES_GATEWAY_PROVIDER_CONFIG = JSON.stringify(provider);
  const registrations = [];
  const hooks = {};
  const api = { registerProvider: (name, config) => registrations.push({ name, ...config }), on: (event, handler) => { hooks[event] = handler; } };
  return { api, registrations, hooks };
}
const provider = { name: "managed-test", api: "openai-completions", managed: true, models: [{ id: "virtual", name: "Virtual", reasoning: false, input: ["text"], contextWindow: 1000000, maxTokens: 128000, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }] };

test("managed catalog registers only the launch provider and retains nonce on session start", () => {
  const { api, registrations, hooks } = setup(provider);
  gateway(api);
  assert.equal(registrations.length, 1);
  assert.equal(registrations[0].name, "managed-test");
  assert.equal(registrations[0].apiKey, "tapes-platform-managed");
  assert.deepEqual(registrations[0].models, provider.models);
  assert.equal(registrations[0].baseUrl, process.env.TAPES_GATEWAY_URL);
  assert.equal(process.env.TAPES_GATEWAY_NONCE, undefined);
  assert.equal(process.env.TAPES_GATEWAY_PROVIDER_CONFIG, undefined);
  hooks.session_start({}, { sessionManager: { getSessionId: () => "session-1" }, ui: { setStatus() {} } });
  assert.deepEqual(registrations[1].headers, { "x-tapes-gateway-nonce": "nonce", "X-Tapes-Harness-Id": "pi", "X-Tapes-Harness-Session-Id": "session-1" });
  const warnings = [];
  hooks.model_select({ model: { provider: "anthropic" } }, { ui: { notify: message => warnings.push(message) } });
  assert.match(warnings[0], /normal endpoint/);
});

test("ordinary launches retain the original three provider registrations", () => {
  const { api, registrations } = setup();
  gateway(api);
  assert.deepEqual(registrations.map(r => r.name), ["anthropic", "openai", "openai-codex"]);
  assert.ok(registrations.every(r => r.apiKey === undefined && r.models === undefined));
});

test("without a gateway the extension is inert and consumes the nonce", () => {
  const { api, registrations } = setup();
  delete process.env.TAPES_GATEWAY_URL;
  gateway(api);
  assert.equal(registrations.length, 0);
  assert.equal(process.env.TAPES_GATEWAY_NONCE, undefined);
});

test("invalid catalogs cannot silently replace ordinary providers", () => {
  for (const value of [null, {}, { ...provider, name: undefined }, { ...provider, name: 123 }, { ...provider, name: "openai" }, { ...provider, models: [] }, { ...provider, api: "other" }]) {
    const { api, registrations } = setup(value);
    assert.throws(() => gateway(api), /Invalid launch provider/);
    assert.equal(registrations.length, 0);
  }
});

test("malformed model entries fail before any provider registration", () => {
  const model = provider.models[0];
  const invalid = [null, {}, [], "model", 42,
    ...Object.keys(model).map(key => ({ ...model, [key]: undefined })),
    { ...model, id: " " }, { ...model, name: 42 }, { ...model, reasoning: "false" },
    { ...model, input: [] }, { ...model, input: ["audio"] },
    { ...model, contextWindow: 0 }, { ...model, contextWindow: 1.5 },
    { ...model, maxTokens: -1 }, { ...model, maxTokens: "128000" },
    { ...model, cost: null }, { ...model, cost: {} },
    ...Object.keys(model.cost).flatMap(key => [
      { ...model, cost: { ...model.cost, [key]: -1 } },
      { ...model, cost: { ...model.cost, [key]: "0" } },
      { ...model, cost: { ...model.cost, [key]: Infinity } },
    ]),
  ];
  for (const entry of invalid) {
    const { api, registrations } = setup({ ...provider, models: [model, entry] });
    assert.throws(() => gateway(api), /Invalid launch provider/, JSON.stringify(entry));
    assert.equal(registrations.length, 0);
  }
});

test("valid multi-model catalogs retain optional model settings", () => {
  const models = [provider.models[0], { ...provider.models[0], id: "vision", reasoning: true,
    input: ["text", "image"], cost: { input: 1, output: 2, cacheRead: 0.1, cacheWrite: 0.5 },
    compat: { supportsStore: false },
  }];
  const { api, registrations } = setup({ ...provider, models });
  gateway(api);
  assert.deepEqual(registrations[0].models, models);
});
