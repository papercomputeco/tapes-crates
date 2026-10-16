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

// Whether the proxy serves each captured provider on its own route.
//
// This file registers three providers and a proxy may only be able to carry
// one. When it can carry one, all three still point at it, and a session on
// either of the other two is forwarded to a host that has never heard of the
// route: the request fails outright, and the failure is attributed to the model
// rather than to capture. The provider is knowable here and nowhere downstream,
// so when the launcher says its proxy routes per provider, each registration
// says which provider it is.
//
// Unset or empty is the single-upstream shape, which is what a launcher that
// predates this variable asks for by saying nothing — its requests are
// unchanged, down to the path.
const GATEWAY_PROVIDER_ROUTES_ENV = "TAPES_GATEWAY_PROVIDER_ROUTES";
const GATEWAY_PROVIDER_CONFIG_ENV = "TAPES_GATEWAY_PROVIDER_CONFIG";

type LaunchProvider = Pick<Parameters<ExtensionAPI["registerProvider"]>[1], "api" | "models"> & {
  name: string;
  managed: boolean;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

// JSON crosses a runtime boundary: the TypeScript provider type alone cannot
// protect Pi's model discovery from null entries or missing required fields.
function validLaunchModel(value: unknown): boolean {
  if (!isRecord(value)) return false;
  const cost = value.cost;
  return typeof value.id === "string" && value.id.trim().length > 0 &&
    typeof value.name === "string" && value.name.trim().length > 0 &&
    typeof value.reasoning === "boolean" &&
    Array.isArray(value.input) && value.input.length > 0 &&
    value.input.every(input => input === "text" || input === "image") &&
    typeof value.contextWindow === "number" && Number.isSafeInteger(value.contextWindow) && value.contextWindow > 0 &&
    typeof value.maxTokens === "number" && Number.isSafeInteger(value.maxTokens) && value.maxTokens > 0 &&
    isRecord(cost) && ["input", "output", "cacheRead", "cacheWrite"].every(key =>
      typeof cost[key] === "number" && Number.isFinite(cost[key]) && cost[key] >= 0);
}

// Where a labelled request puts the provider name. Underscore-led so it cannot
// collide with a path a provider's own API serves.
const PROVIDER_ROUTE_PREFIX = "/_tapes/provider";

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

// The opt-out from refusing uncaptured requests, read at request time. While
// this extension routes a session through a capture proxy, a model whose
// requests would go anywhere else — a provider the proxy does not front, a
// `models.json` provider with its own endpoint — is refused rather than sent
// uncaptured. Exactly `1` restores the old behaviour for this process: those
// requests go to their own endpoint, with a warning, and without any
// `x-tapes-*` header or the nonce echo. Any other value, or none,
// keeps the refusal; a typo must not quietly turn capture off.
const ALLOW_UNCAPTURED_ENV = "TAPES_ALLOW_UNCAPTURED";

// The envelope's byte budget, mirroring tapes-capture: the whole `x-tapes-*`
// set within 8 KiB of name and value bytes. Optional fields are given up in
// this order when a value (in practice, a very long working directory) would
// push past it; the harness id and session id always stay.
const X_TAPES_TOTAL_BUDGET = 8 * 1024;
const OPTIONAL_DROP_ORDER = ["X-Tapes-Cwd", "X-Tapes-Harness-Version"] as const;

function normalizeBaseUrl(url: string): string {
  const trimmed = url.replace(/\/+$/, "");
  return trimmed.startsWith("http://") || trimmed.startsWith("https://") ? trimmed : `http://${trimmed}`;
}

function isSchemaProvider(provider: string): provider is (typeof SCHEMA_PROVIDERS)[number] {
  return (SCHEMA_PROVIDERS as readonly string[]).includes(provider);
}

// A header value usable verbatim: visible ASCII only. Anything else is dropped
// rather than sent — absent means no evidence, and a control byte in a header
// would be a smuggling vector.
function headerSafe(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed.length > 0 && /^[\x21-\x7e]+$/.test(trimmed) ? trimmed : undefined;
}

// The working directory in the envelope's encoding: UTF-8, with every byte
// outside visible ASCII — and `%`, `"` and `\` — percent-encoded, upper-case
// hex. The same escape set as the Rust producer and the hermes plugin, so one
// directory is one header value whichever producer stamped it.
export function encodeCwd(cwd: string): string {
  let out = "";
  for (const byte of new TextEncoder().encode(cwd)) {
    const safe = byte >= 0x21 && byte <= 0x7e && byte !== 0x25 && byte !== 0x22 && byte !== 0x5c;
    out += safe ? String.fromCharCode(byte) : `%${byte.toString(16).toUpperCase().padStart(2, "0")}`;
  }
  return out;
}

// Whether `candidate` is an address on the capture proxy at `gatewayUrl`. A
// boundary comparison on parsed URLs, not a string prefix: a prefix test would
// accept `https://gw.example.attacker.invalid` for a proxy at
// `https://gw.example`. Scheme, host and port must match, and the path must be
// the proxy's mount or lie beneath it on a separator.
export function isGatewayAddress(candidate: unknown, gatewayUrl: string): boolean {
  if (typeof candidate !== "string" || candidate.trim().length === 0) return false;
  let url: URL;
  let gateway: URL;
  try {
    url = new URL(candidate.trim());
    gateway = new URL(gatewayUrl);
  } catch {
    return false;
  }
  if ((url.protocol !== "http:" && url.protocol !== "https:") || url.hostname.length === 0) return false;
  // `URL` already folds a scheme's default port to "", so equal `host`s are
  // equal host-and-port pairs.
  if (url.protocol !== gateway.protocol || url.host !== gateway.host) return false;
  const mount = gateway.pathname.replace(/\/+$/, "");
  const path = url.pathname.replace(/\/+$/, "");
  return path === mount || path.startsWith(`${mount}/`);
}

// The one line a refused request reports. It names the provider and the host
// it would have reached — never a path, query, or credential.
function refusalMessage(provider: string, baseUrl: unknown): string {
  let host = "no endpoint";
  try {
    if (typeof baseUrl === "string") host = new URL(baseUrl).host || host;
  } catch {
    // An unparseable endpoint is still refused; it just has no host to name.
  }
  return `This model provider bypasses capture (${provider}, ${host}). ` +
    `Use a model routed through the tapes gateway, or set ${ALLOW_UNCAPTURED_ENV}=1 to allow uncaptured calls.`;
}

// What `before_provider_request` returns in place of a request that must not
// be sent. pi offers extensions no way to cancel a provider request — an error
// thrown from a handler is reported and the request goes out regardless — but
// it does use a replacement payload, and building the request reads fields
// from it. Every read but the thenable probe pi makes while awaiting the
// handler throws `error`, so the request fails before anything is sent, with
// this message as the turn's error.
function refusingPayload(error: Error): object {
  const refuse = (): never => {
    throw error;
  };
  return new Proxy(
    {},
    {
      get: (_target, key) => (key === "then" ? undefined : refuse()),
      has: refuse,
      ownKeys: refuse,
      getOwnPropertyDescriptor: refuse,
      set: refuse,
      deleteProperty: refuse,
      defineProperty: refuse,
    },
  );
}

type SelectedModel = { provider?: unknown; baseUrl?: unknown } | undefined;

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
  // Optional launch-local model catalog. A dedicated provider avoids replacing
  // the user's ordinary provider definitions or writing their auth/models files.
  const rawProvider = process.env[GATEWAY_PROVIDER_CONFIG_ENV];
  delete process.env[GATEWAY_PROVIDER_CONFIG_ENV];
  const provider: LaunchProvider | undefined = rawProvider ? JSON.parse(rawProvider) : undefined;
  if (rawProvider && (!provider ||
    typeof provider.name !== "string" || !/^[a-z][a-z0-9-]*$/.test(provider.name) ||
    (CAPTURED_PROVIDERS as readonly string[]).includes(provider.name) ||
    provider.api !== "openai-completions" || !Array.isArray(provider.models) ||
    provider.models.length === 0 || !provider.models.every(validLaunchModel) ||
    typeof provider.managed !== "boolean"
  )) {
    throw new Error("Invalid launch provider configuration");
  }

  // Any non-empty value asks for per-provider routes; unset and empty are the
  // single-upstream shape. Deliberately not a parse of `true`/`1`: a launcher
  // and an installed extension disagreeing about which spellings count is the
  // kind of skew that shows up as a dead session, and "did the launcher set it"
  // is the only question with one answer.
  const providerRoutes = !!process.env[GATEWAY_PROVIDER_ROUTES_ENV];

  // The base URL a provider's requests are built from. With per-provider routes
  // that is the proxy plus this provider's label, which is the only place the
  // provider is still known: pi appends its own path to whatever base a
  // provider was registered with, and sends no header naming it.
  const providerBaseUrl = (provider: string): string =>
    providerRoutes ? `${baseUrl}${PROVIDER_ROUTE_PREFIX}/${provider}` : baseUrl;

  // pi's version, for the envelope. pi serves its own package to extensions as
  // a module; asking for it at runtime keeps this file free of a static import
  // that only pi can resolve. Started now, awaited before the first envelope:
  // nothing a version is needed for happens before `session_start`.
  let harnessVersion: string | undefined;
  const versionReady: Promise<void> = import("@earendil-works/pi-coding-agent").then(
    (module) => {
      harnessVersion = headerSafe((module as { VERSION?: unknown }).VERSION);
    },
    () => undefined,
  );

  // Presentation, resolved once per session from the launch's environment.
  // Empty is treated as unset for the two that have meaningful defaults; an
  // explicitly empty suffix is a real value and stays empty.
  const statusLabel = process.env[GATEWAY_LABEL_ENV] || DEFAULT_LABEL;
  const statusSuffix = process.env[GATEWAY_LABEL_SUFFIX_ENV] ?? "";
  const schemaRemedy = process.env[GATEWAY_REMEDY_ENV] || DEFAULT_REMEDY;

  // The envelope pi stamps on its own behalf, within the shared byte budget.
  // pi is the self-attributing harness: the capture client cannot recover this
  // session's identity from a peer-PID lookup, so what these headers carry is
  // the only attribution the turn will ever have.
  const envelopeFor = (harnessSessionId: string, cwd: string | undefined): Record<string, string> => {
    const envelope: Record<string, string> = {
      "X-Tapes-Harness-Id": "pi",
      "X-Tapes-Harness-Session-Id": harnessSessionId,
    };
    if (harnessVersion) envelope["X-Tapes-Harness-Version"] = harnessVersion;
    if (cwd) envelope["X-Tapes-Cwd"] = encodeCwd(cwd);
    const size = () => Object.entries(envelope).reduce((sum, [name, value]) => sum + name.length + value.length, 0);
    for (const optional of OPTIONAL_DROP_ORDER) {
      if (size() <= X_TAPES_TOTAL_BUDGET) break;
      delete envelope[optional];
    }
    return envelope;
  };

  const registerCapturedProviders = (harnessSessionId?: string, cwd?: string) => {
    // The nonce echo rides alongside the envelope — without the echo the proxy
    // has no way to distinguish this extension from a subprocess of the
    // harness forging an envelope.
    const envelope = harnessSessionId ? envelopeFor(harnessSessionId, cwd) : undefined;
    const headers = {
      ...(nonce ? { [GATEWAY_NONCE_HEADER]: nonce } : undefined),
      ...envelope,
    };

    if (provider) {
      pi.registerProvider(provider.name, {
        api: provider.api, models: provider.models, baseUrl, headers,
        // Pi requires a nonempty credential even when the gateway owns auth.
        // This sentinel is not a provider key and is scoped to this provider.
        ...(provider.managed ? { apiKey: "tapes-platform-managed" } : {}),
      });
    } else {
      for (const provider of CAPTURED_PROVIDERS) {
        const url = providerBaseUrl(provider);
        pi.registerProvider(provider, Object.keys(headers).length > 0 ? { baseUrl: url, headers } : { baseUrl: url });
      }
    }
  };

  // Register immediately so the proxied providers are available during model
  // discovery, which runs before any session exists. Once pi binds a concrete
  // session, `session_start` re-registers the same providers with pi's native
  // stable session id, which is what ingest files the turns under.
  registerCapturedProviders();

  const activeSchema = process.env[GATEWAY_SCHEMA_ENV] ?? "unselected";
  const schemaProvider = isSchemaProvider(activeSchema) ? activeSchema : undefined;

  // Whether a model's requests reach the capture proxy. Decided by where they
  // go rather than by provider name: a provider this file registered points at
  // the proxy, and so does a model a launcher declared for it (in a session
  // `models.json`, under a registered provider) — while a provider nobody
  // redirected, or a model carrying its own endpoint, does not.
  const routed = (model: SelectedModel): boolean => !!model && isGatewayAddress(model.baseUrl, baseUrl);
  const allowUncaptured = (): boolean => process.env[ALLOW_UNCAPTURED_ENV] === "1";
  const providerOf = (model: SelectedModel): string =>
    typeof model?.provider === "string" ? model.provider : "unknown";

  pi.on("session_start", async (_event, ctx) => {
    await versionReady;
    registerCapturedProviders(ctx.sessionManager.getSessionId(), ctx.cwd || process.cwd());
    ctx.ui.setStatus(statusLabel, `${statusLabel}:${activeSchema}${statusSuffix}`);
    if (ctx.model && !routed(ctx.model)) {
      ctx.ui.notify(refusalMessage(providerOf(ctx.model), ctx.model.baseUrl), allowUncaptured() ? "warning" : "error");
    }
  });

  // The refusal itself. pi asks this hook for each provider request's payload;
  // a request on a model that would bypass the proxy gets a payload that fails
  // the request before it is sent. Judged against the session's current model,
  // which is the model pi's own turns use.
  pi.on("before_provider_request", (_event, ctx) => {
    const model: SelectedModel = ctx.model;
    if (!model || routed(model) || allowUncaptured()) {
      return undefined;
    }
    return refusingPayload(new Error(refusalMessage(providerOf(model), model.baseUrl)));
  });

  // An opted-out request leaves bare. The envelope and the nonce echo are
  // registered as *provider* headers, and pi applies a provider's headers to
  // every model under it — including a `models.json` model under `openai` with
  // its own `baseUrl`. So a request this extension lets through uncaptured
  // would otherwise hand its session identity, and the proxy's secret, to
  // whatever host that model names. pi passes the merged header map through
  // this hook by reference; deleting from it is what removes them. Judged on
  // the same model the refusal above is, and only for a model that is known
  // and off the proxy: a routed request needs both.
  pi.on("before_provider_headers", (event, ctx) => {
    const model: SelectedModel = ctx.model;
    const headers = (event as { headers?: unknown }).headers;
    if (!model || routed(model) || typeof headers !== "object" || headers === null) {
      return;
    }
    for (const name of Object.keys(headers)) {
      if (name.toLowerCase().startsWith("x-tapes-")) {
        delete (headers as Record<string, unknown>)[name];
      }
    }
  });

  pi.on("model_select", (event, ctx) => {
    const selected: SelectedModel = event.model;
    const selectedProvider = providerOf(selected);

    if (!routed(selected)) {
      if (allowUncaptured()) {
        ctx.ui.notify(
          `${selectedProvider} will use its own endpoint and will not be captured (${ALLOW_UNCAPTURED_ENV}=1).`,
          "warning",
        );
      } else {
        ctx.ui.notify(refusalMessage(selectedProvider, selected?.baseUrl), "error");
      }
      return;
    }

    if (provider || selectedProvider === "openai-codex") {
      return;
    }

    // The active schema only constrains what can be captured while one upstream
    // serves everything. With per-provider routes the proxy carries all three,
    // so warning about the "wrong" one would tell a user their working session
    // is broken.
    if (!providerRoutes && schemaProvider && isSchemaProvider(selectedProvider) && selectedProvider !== schemaProvider) {
      ctx.ui.notify(
        `The capture proxy is routing the ${schemaProvider} schema; the selected model's provider is ` +
          `${selectedProvider}. ${schemaRemedy}`,
        "warning",
      );
    }
  });
}
