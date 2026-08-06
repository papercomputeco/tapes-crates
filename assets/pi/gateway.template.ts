import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

// ---------------------------------------------------------------------------
// Consumer slots.
//
// Every slot below is the *entire* string literal of a `const` declaration in
// this block, and nothing in the body of the extension reads a slot any other
// way. That is the whole safety property of this template: a rendered value can
// change where the extension points when nothing configured it, and what it
// says to the user — and nothing else. The capture-nonce handling further down
// is out of every consumer's reach by construction, not by convention.
//
// Adding a slot outside this block, or interpolating one into a template
// literal rather than declaring it here, breaks that property. If a slot would
// change what the extension *does*, it does not belong here at all: this file
// is one implementation with two brandings, not a fork with a shared prefix.
// ---------------------------------------------------------------------------

// Where to send captured traffic when GATEWAY_URL_ENV names nothing.
//
// Empty — the normal value — means "nowhere": the extension leaves every
// provider on pi's own endpoint and does nothing at all. That default is not
// timidity. This file installs into pi's *global* auto-discovery directory, so
// it loads for every pi session on the machine, including the ones nobody is
// capturing; and a capture proxy picks its port per launch, so no fixed address
// could be right anyway. A consumer that fills this slot is stating that it
// runs a long-lived proxy at a known address and wants uncaptured sessions
// routed through it too, and it owns that consequence.
const DEFAULT_GATEWAY_URL = "__TAPES_DEFAULT_GATEWAY_URL__";

// The key this extension's status entry is registered under, and the prefix of
// the label shown in it. A short product word.
const STATUS_KEY = "__TAPES_STATUS_KEY__";

// Appended to the status label after the active schema. Normally empty.
const STATUS_SUFFIX = "__TAPES_STATUS_SUFFIX__";

// The sentence appended to the schema-mismatch warning, telling the user how to
// resolve it with the installing product's own tools. The diagnosis before it
// is this file's; only the remedy is the consumer's, because only the consumer
// knows what command switches its proxy.
const SCHEMA_MISMATCH_REMEDY = "__TAPES_SCHEMA_MISMATCH_REMEDY__";

// ---------------------------------------------------------------------------
// Below this line nothing is a slot.
// ---------------------------------------------------------------------------

// The providers whose traffic a capture proxy can carry. pi's other providers
// keep their own endpoints: redirecting a provider the proxy does not front
// would break the session rather than capture it.
const CAPTURED_PROVIDERS = ["anthropic", "openai", "openai-codex"] as const;

// The subset a capture proxy selects *between* when it fronts one upstream
// schema at a time. `openai-codex` is deliberately absent: it rides its own
// route and is never the "active schema" the proxy is switched to.
const SCHEMA_PROVIDERS = ["anthropic", "openai"] as const;

// Names the host capture client sets before launching pi. They are the whole
// contract between this asset and whoever installed it — see the Rust
// constants in `crate::plugin`, which the crate's tests pin against this file.
const GATEWAY_URL_ENV = "TAPES_GATEWAY_URL";
const GATEWAY_SCHEMA_ENV = "TAPES_GATEWAY_SCHEMA";
const GATEWAY_NONCE_ENV = "TAPES_GATEWAY_NONCE";

// The header the nonce is echoed back in. A per-launch secret the capture
// client generated: the proxy's peer-PID ancestry check cannot tell this
// extension's requests apart from requests made by the harness's own
// subprocesses (a shell tool's child is a descendant too), and the echoed
// nonce is what does. Only this process was handed the value, the proxy
// validates and strips it before forwarding, and it must never be written
// anywhere else — not logged, not surfaced in the UI.
const GATEWAY_NONCE_HEADER = "x-tapes-gateway-nonce";

function normalizeBaseUrl(url: string): string {
  const trimmed = url.replace(/\/+$/, "");
  return trimmed.startsWith("http://") || trimmed.startsWith("https://") ? trimmed : `http://${trimmed}`;
}

function isSchemaProvider(provider: string): provider is (typeof SCHEMA_PROVIDERS)[number] {
  return (SCHEMA_PROVIDERS as readonly string[]).includes(provider);
}

export default function (pi: ExtensionAPI) {
  // Take the launch secret out of the environment before anything else runs.
  // Extensions load before any tool executes, and subprocesses the harness
  // later spawns inherit the harness's *current* environment — so reading the
  // nonce once and deleting the variable here means those children never
  // receive it, and the value survives only in this closure. Deleted even when
  // no gateway URL is set: an inert extension must not leave the secret lying
  // in the environment either. The URL and schema variables stay — they are
  // addresses, not secrets, and other tooling may legitimately read them.
  const nonce = process.env[GATEWAY_NONCE_ENV];
  delete process.env[GATEWAY_NONCE_ENV];

  const rawBaseUrl = process.env[GATEWAY_URL_ENV] ?? DEFAULT_GATEWAY_URL;

  // Nothing to route through: this session is not being captured, so leave
  // every provider on pi's own endpoint and return. Reached whenever the
  // launcher set no gateway and the rendering carries no default — see
  // DEFAULT_GATEWAY_URL above for why that is the ordinary case.
  if (!rawBaseUrl) {
    return;
  }

  const baseUrl = normalizeBaseUrl(rawBaseUrl);

  const registerCapturedProviders = (harnessSessionId?: string) => {
    // The envelope pi stamps on its own behalf. pi is the self-attributing
    // harness: the capture client cannot recover this session's identity from
    // a peer-PID lookup, so what these headers carry is the only attribution
    // the turn will ever have. The nonce echo rides alongside it — without
    // the echo the proxy has no way to distinguish this extension from a
    // subprocess of the harness forging an envelope.
    const envelope = harnessSessionId
      ? {
          "X-Tapes-Harness-Id": "pi",
          "X-Tapes-Harness-Session-Id": harnessSessionId,
        }
      : undefined;
    const headers = {
      ...(nonce ? { [GATEWAY_NONCE_HEADER]: nonce } : undefined),
      ...envelope,
    };

    for (const provider of CAPTURED_PROVIDERS) {
      pi.registerProvider(provider, Object.keys(headers).length > 0 ? { baseUrl, headers } : { baseUrl });
    }
  };

  // Register immediately so the proxied providers are available during model
  // discovery, which runs before any session exists. Once pi binds a concrete
  // session, `session_start` re-registers the same providers with pi's native
  // stable session id, which is what ingest files the turns under.
  registerCapturedProviders();

  const activeSchema = process.env[GATEWAY_SCHEMA_ENV] ?? "unselected";
  const schemaProvider = isSchemaProvider(activeSchema) ? activeSchema : undefined;

  pi.on("session_start", (_event, ctx) => {
    registerCapturedProviders(ctx.sessionManager.getSessionId());
    ctx.ui.setStatus(STATUS_KEY, `${STATUS_KEY}:${activeSchema}${STATUS_SUFFIX}`);
  });

  pi.on("model_select", (event, ctx) => {
    const selectedProvider = event.model.provider;

    if (selectedProvider === "openai-codex") {
      return;
    }

    if (schemaProvider && isSchemaProvider(selectedProvider) && selectedProvider !== schemaProvider) {
      ctx.ui.notify(
        `The capture proxy is routing the ${schemaProvider} schema; the selected model's provider is ` +
          `${selectedProvider}. ${SCHEMA_MISMATCH_REMEDY}`,
        "warning",
      );
      return;
    }

    if (!(CAPTURED_PROVIDERS as readonly string[]).includes(selectedProvider)) {
      ctx.ui.notify(
        `Capture covers pi's Anthropic, OpenAI, and OpenAI Codex providers; ${selectedProvider} will use ` +
          "pi's normal endpoint and will not be captured.",
        "warning",
      );
    }
  });
}
