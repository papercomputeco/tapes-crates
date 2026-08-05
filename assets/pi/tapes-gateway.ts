import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

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
  const rawBaseUrl = process.env[GATEWAY_URL_ENV];

  // No capture proxy configured, so this session is not being captured: leave
  // every provider on pi's own endpoint and return.
  //
  // This is why there is no default address. The extension installs into pi's
  // global auto-discovery directory, so it loads for *every* pi session on the
  // machine, not only the ones launched under capture. A built-in default would
  // silently route ordinary sessions at whatever happened to be listening on
  // that port — and a capture proxy's port is chosen per launch, so no fixed
  // default could be right anyway.
  if (!rawBaseUrl) {
    return;
  }

  const baseUrl = normalizeBaseUrl(rawBaseUrl);

  // Absent when the launching client predates the nonce contract; then no
  // header is sent and the proxy applies whatever policy it has without one.
  const nonce = process.env[GATEWAY_NONCE_ENV];

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
    ctx.ui.setStatus("tapes", `tapes:${activeSchema}`);
  });

  pi.on("model_select", (event, ctx) => {
    const selectedProvider = event.model.provider;

    if (selectedProvider === "openai-codex") {
      return;
    }

    if (schemaProvider && isSchemaProvider(selectedProvider) && selectedProvider !== schemaProvider) {
      ctx.ui.notify(
        `The capture proxy is routing the ${schemaProvider} schema; the selected model's provider is ` +
          `${selectedProvider}. Point ${GATEWAY_URL_ENV} at a proxy serving ${selectedProvider}, or switch ` +
          "that proxy's active schema, if requests fail.",
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
