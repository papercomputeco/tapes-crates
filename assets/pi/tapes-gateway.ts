import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

// ---------------------------------------------------------------------------
// One file, whoever installed it.
//
// pi auto-loads *every* file in `~/.pi/agent/extensions/`, into one process.
// Two products that each install their own copy of this extension therefore get
// two copies loaded, and the two contend over every resource the file touches:
// the launch nonce (read once and deleted, so the second reader finds nothing)
// and, more decisively, the provider registrations — both copies call
// `registerProvider` for the same three providers, and the last write wins. The
// copy that lost the nonce still registers, without the echo, and the proxy
// then cannot tell a real launch from a forged envelope: both products' pi
// sessions file as `unknown`, with no error anywhere.
//
// So this file is not per-product. Every client installs *these* bytes to
// *this* path, which is what makes a second reader impossible rather than
// merely coordinated. What a product legitimately says differently — what its
// status entry is called, what it tells a user to run — it says at runtime,
// through the environment of the launch it owns, and never by shipping
// different bytes.
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
//
// Shared spellings, deliberately. A per-product name would only be needed if a
// second installed copy could read them, and installing one file to one path is
// what removes that copy.
const GATEWAY_URL_ENV = "TAPES_GATEWAY_URL";
const GATEWAY_SCHEMA_ENV = "TAPES_GATEWAY_SCHEMA";
const GATEWAY_NONCE_ENV = "TAPES_GATEWAY_NONCE";

// The launching product's own presentation, carried the same way — set by the
// client that launched this session, for the length of that session.
//
// Everything read from these three is display text. It reaches `setStatus` and
// `notify` and nothing else: not the nonce, not the envelope, not the provider
// registration. That is the same containment a rendered slot used to buy, at
// runtime and without a second file to render.
const GATEWAY_LABEL_ENV = "TAPES_GATEWAY_LABEL";
const GATEWAY_LABEL_SUFFIX_ENV = "TAPES_GATEWAY_LABEL_SUFFIX";
const GATEWAY_REMEDY_ENV = "TAPES_GATEWAY_REMEDY";

// What to present when the launcher named nothing. Vendor-neutral by
// obligation: these bytes install into every client, so the fallback phrases
// itself in terms of the environment contract rather than any product's CLI.
const DEFAULT_LABEL = "tapes";
const DEFAULT_REMEDY =
  "Point TAPES_GATEWAY_URL at a proxy serving that provider, or switch that proxy's active schema, if requests fail.";

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
  // in the environment either. The URL, schema, and presentation variables
  // stay — they are addresses and display text, not secrets, and other tooling
  // may legitimately read them.
  const nonce = process.env[GATEWAY_NONCE_ENV];
  delete process.env[GATEWAY_NONCE_ENV];

  const rawBaseUrl = process.env[GATEWAY_URL_ENV];

  // Nothing to route through: this session is not being captured, so leave
  // every provider on pi's own endpoint and return. This file installs into
  // pi's *global* auto-discovery directory, so it loads for every pi session on
  // the machine — including every session nobody launched under capture — and
  // staying inert for those is the whole reason the redirect is conditional on
  // the environment. There is no built-in fallback address on purpose: a fixed
  // one would belong to whichever product wrote it, and these bytes belong to
  // every product that installs them.
  if (!rawBaseUrl) {
    return;
  }

  const baseUrl = normalizeBaseUrl(rawBaseUrl);

  // Presentation, resolved once per session from the launch's environment.
  // Empty is treated as unset for the two that have meaningful defaults; an
  // explicitly empty suffix is a real value and stays empty.
  const statusLabel = process.env[GATEWAY_LABEL_ENV] || DEFAULT_LABEL;
  const statusSuffix = process.env[GATEWAY_LABEL_SUFFIX_ENV] ?? "";
  const schemaRemedy = process.env[GATEWAY_REMEDY_ENV] || DEFAULT_REMEDY;

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
    ctx.ui.setStatus(statusLabel, `${statusLabel}:${activeSchema}${statusSuffix}`);
  });

  pi.on("model_select", (event, ctx) => {
    const selectedProvider = event.model.provider;

    if (selectedProvider === "openai-codex") {
      return;
    }

    if (schemaProvider && isSchemaProvider(selectedProvider) && selectedProvider !== schemaProvider) {
      ctx.ui.notify(
        `The capture proxy is routing the ${schemaProvider} schema; the selected model's provider is ` +
          `${selectedProvider}. ${schemaRemedy}`,
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
