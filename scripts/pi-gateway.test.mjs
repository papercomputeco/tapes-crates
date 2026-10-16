// Node 24+: node --experimental-strip-types --test scripts/pi-gateway.test.mjs
import { test, afterEach } from "node:test";
import assert from "node:assert/strict";
import gateway, { encodeCwd, isGatewayAddress } from "../crates/tapes-harnesses/assets/pi/tapes-gateway.ts";

const keys = ["TAPES_GATEWAY_URL", "TAPES_GATEWAY_NONCE", "TAPES_GATEWAY_PROVIDER_CONFIG", "TAPES_GATEWAY_PROVIDER_ROUTES", "TAPES_GATEWAY_SCHEMA", "TAPES_ALLOW_UNCAPTURED"];
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

const GATEWAY = "http://127.0.0.1:1234/launches/pilot/openai";
const ctxFor = (model, ui = {}) => ({
  sessionManager: { getSessionId: () => "session-1" },
  cwd: "/work/dir",
  model,
  ui: { setStatus() {}, notify() {}, ...ui },
});

test("managed catalog registers only the launch provider and retains nonce on session start", async () => {
  const { api, registrations, hooks } = setup(provider);
  gateway(api);
  assert.equal(registrations.length, 1);
  assert.equal(registrations[0].name, "managed-test");
  assert.equal(registrations[0].apiKey, "tapes-platform-managed");
  assert.deepEqual(registrations[0].models, provider.models);
  assert.equal(registrations[0].baseUrl, process.env.TAPES_GATEWAY_URL);
  assert.equal(process.env.TAPES_GATEWAY_NONCE, undefined);
  assert.equal(process.env.TAPES_GATEWAY_PROVIDER_CONFIG, undefined);
  await hooks.session_start({}, ctxFor({ provider: "managed-test", baseUrl: GATEWAY }));
  assert.deepEqual(registrations[1].headers, {
    "x-tapes-gateway-nonce": "nonce", "X-Tapes-Harness-Id": "pi", "X-Tapes-Harness-Session-Id": "session-1",
    "X-Tapes-Cwd": "/work/dir",
  });
  const notices = [];
  hooks.model_select({ model: { provider: "anthropic", baseUrl: "https://api.anthropic.com" } },
    { ui: { notify: (message, level) => notices.push({ message, level }) } });
  assert.equal(notices[0].level, "error");
  assert.match(notices[0].message, /bypasses capture \(anthropic, api\.anthropic\.com\)/);
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

// The envelope's cwd encoding, against the vectors the Rust producer and the
// hermes plugin are pinned to: one directory, one header value.
test("cwd is percent-encoded exactly like the other producers", () => {
  assert.equal(encodeCwd("/Users/matt/code"), "/Users/matt/code");
  assert.equal(encodeCwd("/Users/matt/go+rust"), "/Users/matt/go+rust");
  assert.equal(encodeCwd("/Users/松本/code"), "/Users/%E6%9D%BE%E6%9C%AC/code");
  assert.equal(encodeCwd("/Users/matt\nwith-injection: yes"), "/Users/matt%0Awith-injection:%20yes");
  assert.equal(encodeCwd('/tmp/a b/%/"/\\/ü'), "/tmp/a%20b/%25/%22/%5C/%C3%BC");
  assert.equal(encodeCwd("/x\x7f"), "/x%7F");
});

test("session start stamps cwd and drops it before the session id when over budget", async () => {
  const { api, registrations, hooks } = setup();
  gateway(api);
  await hooks.session_start({}, ctxFor({ provider: "openai", baseUrl: GATEWAY }));
  const headers = registrations.at(-1).headers;
  assert.equal(headers["X-Tapes-Cwd"], "/work/dir");
  assert.equal(headers["X-Tapes-Harness-Session-Id"], "session-1");
  // No pi package to ask outside pi: the version is absent rather than guessed.
  assert.equal(headers["X-Tapes-Harness-Version"], undefined);

  const huge = setup();
  gateway(huge.api);
  await huge.hooks.session_start({}, { ...ctxFor({ provider: "openai", baseUrl: GATEWAY }), cwd: "/" + "d".repeat(9000) });
  const fitted = huge.registrations.at(-1).headers;
  assert.equal(fitted["X-Tapes-Cwd"], undefined);
  assert.equal(fitted["X-Tapes-Harness-Session-Id"], "session-1");
  assert.equal(fitted["x-tapes-gateway-nonce"], "nonce");
});

test("gateway addresses compare on parsed scheme, host, port and path boundary", () => {
  const gw = "http://127.0.0.1:1234/capture";
  assert.ok(isGatewayAddress("http://127.0.0.1:1234/capture", gw));
  assert.ok(isGatewayAddress("http://127.0.0.1:1234/capture/", gw));
  assert.ok(isGatewayAddress("http://127.0.0.1:1234/capture/_tapes/provider/openai", gw));
  for (const other of [
    "http://127.0.0.1:1234/capture-elsewhere",
    "http://127.0.0.1:1234/",
    "http://127.0.0.1:12345/capture",
    "https://127.0.0.1:1234/capture",
    "http://127.0.0.10:1234/capture",
    "http://127.0.0.1.attacker.invalid:1234/capture",
    "ftp://127.0.0.1:1234/capture",
    "not a url", "", undefined, 42,
  ]) {
    assert.equal(isGatewayAddress(other, gw), false, String(other));
  }
  assert.ok(isGatewayAddress("https://gw.example/v1", "https://gw.example:443"));
  assert.equal(isGatewayAddress("https://gw.example.attacker.invalid/v1", "https://gw.example"), false);
  assert.equal(isGatewayAddress("https://attacker-gw.example/v1", "https://gw.example"), false);
});

function payloadFor(hooks, model) {
  return hooks.before_provider_request({ type: "before_provider_request", payload: { model: "m", stream: true } }, { model });
}

test("requests on a model that bypasses the gateway are refused before they are built", () => {
  const { api, hooks } = setup();
  gateway(api);
  // Routed: pi keeps its own payload.
  assert.equal(payloadFor(hooks, { provider: "openai", baseUrl: GATEWAY }), undefined);
  assert.equal(payloadFor(hooks, { provider: "openai", baseUrl: `${GATEWAY}/_tapes/provider/openai` }), undefined);
  // Unknown model: nothing to judge.
  assert.equal(payloadFor(hooks, undefined), undefined);
  // A self-hosted provider with its own endpoint, a lookalike, and no endpoint at all.
  for (const model of [
    { provider: "deepseek", baseUrl: "https://llm.internal.example/v1?key=secret" },
    { provider: "openai", baseUrl: "http://127.0.0.1:1234/launches/pilot/openai-elsewhere" },
    { provider: "custom" },
  ]) {
    const refused = payloadFor(hooks, model);
    assert.notEqual(refused, undefined, JSON.stringify(model));
    // Awaitable without throwing (pi awaits the handler's result)…
    assert.equal(refused.then, undefined);
    // …but every read pi makes to build the request fails with the message.
    assert.throws(() => refused.stream, /bypasses capture/);
    assert.throws(() => JSON.stringify(refused), /TAPES_ALLOW_UNCAPTURED=1/);
    assert.throws(() => Object.keys(refused), /bypasses capture/);
    assert.throws(() => refused.stream, error => !/secret|\/v1/.test(error.message));
  }
});

test("TAPES_ALLOW_UNCAPTURED=1, read per request, restores pass-through with a warning", async () => {
  const { api, hooks } = setup();
  gateway(api);
  const bypass = { provider: "deepseek", baseUrl: "https://llm.internal.example/v1" };
  process.env.TAPES_ALLOW_UNCAPTURED = "true";
  assert.notEqual(payloadFor(hooks, bypass), undefined, "only exactly 1 opts out");
  process.env.TAPES_ALLOW_UNCAPTURED = "1";
  assert.equal(payloadFor(hooks, bypass), undefined);
  const notices = [];
  hooks.model_select({ model: bypass }, { ui: { notify: (message, level) => notices.push({ message, level }) } });
  assert.equal(notices[0].level, "warning");
  assert.match(notices[0].message, /will not be captured/);
  delete process.env.TAPES_ALLOW_UNCAPTURED;
  assert.notEqual(payloadFor(hooks, bypass), undefined);
});

test("an opted-out request to another host carries no envelope and no nonce", async () => {
  const { api, hooks } = setup();
  gateway(api);
  process.env.TAPES_ALLOW_UNCAPTURED = "1";
  const stamped = () => ({
    "x-tapes-gateway-nonce": "nonce", "X-Tapes-Harness-Id": "pi", "X-Tapes-Harness-Session-Id": "session-1",
    "X-Tapes-Cwd": "/work/dir", "Authorization": "Bearer sk-user", "User-Agent": "pi",
  });
  // A model under a redirected provider with its own endpoint: pi applies the
  // provider's headers to it, so the hook is what keeps them off the wire.
  for (const model of [
    { provider: "openai", baseUrl: "https://llm.internal.example/v1" },
    { provider: "deepseek", baseUrl: "https://api.deepseek.example/v1" },
  ]) {
    const headers = stamped();
    await hooks.before_provider_headers({ type: "before_provider_headers", headers }, { model });
    assert.deepEqual(headers, { "Authorization": "Bearer sk-user", "User-Agent": "pi" }, JSON.stringify(model));
  }
  // A routed request keeps both; so does one whose model is unknown.
  for (const model of [{ provider: "openai", baseUrl: GATEWAY }, undefined]) {
    const headers = stamped();
    await hooks.before_provider_headers({ type: "before_provider_headers", headers }, { model });
    assert.deepEqual(headers, stamped(), JSON.stringify(model));
  }
});

test("session start reports a starting model that bypasses the gateway", async () => {
  const { api, hooks } = setup();
  gateway(api);
  const notices = [];
  await hooks.session_start({}, ctxFor({ provider: "deepseek", baseUrl: "https://llm.internal.example/v1" },
    { notify: (message, level) => notices.push({ message, level }) }));
  assert.equal(notices.length, 1);
  assert.equal(notices[0].level, "error");
  const quiet = [];
  await hooks.session_start({}, ctxFor({ provider: "openai", baseUrl: GATEWAY },
    { notify: (message, level) => quiet.push({ message, level }) }));
  assert.equal(quiet.length, 0);
});

test("routed providers keep the schema-mismatch warning and nothing else", () => {
  const { api, hooks } = setup();
  process.env.TAPES_GATEWAY_SCHEMA = "anthropic";
  gateway(api);
  const notices = [];
  const ctx = { ui: { notify: (message, level) => notices.push({ message, level }) } };
  hooks.model_select({ model: { provider: "openai", baseUrl: GATEWAY } }, ctx);
  hooks.model_select({ model: { provider: "anthropic", baseUrl: GATEWAY } }, ctx);
  hooks.model_select({ model: { provider: "openai-codex", baseUrl: GATEWAY } }, ctx);
  assert.equal(notices.length, 1);
  assert.equal(notices[0].level, "warning");
  assert.match(notices[0].message, /routing the anthropic schema/);
});

test("without a gateway no refusal hook is installed", () => {
  const { api, hooks } = setup();
  delete process.env.TAPES_GATEWAY_URL;
  gateway(api);
  assert.deepEqual(Object.keys(hooks), []);
});
