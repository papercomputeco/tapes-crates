// The providers whose traffic a capture proxy can carry. opencode's other
// providers keep their own endpoints: redirecting a provider the proxy does
// not front would break the session rather than capture it. The spellings are
// opencode's provider ids, which are also the schema names a capture proxy
// selects between — for opencode the two sets coincide, because its Anthropic
// and OpenAI providers each speak exactly the schema they are named after.
const CAPTURED_PROVIDERS = ["anthropic", "openai"] as const;

// Names the host capture client sets before launching opencode. They are the
// whole contract between this asset and whoever installed it — see the Rust
// constants in `crate::plugin`, which the crate's tests pin against this file.
const GATEWAY_URL_ENV = "TAPES_GATEWAY_URL";
const GATEWAY_SCHEMA_ENV = "TAPES_GATEWAY_SCHEMA";
const GATEWAY_NONCE_ENV = "TAPES_GATEWAY_NONCE";

// The header the nonce is echoed back in. A per-launch secret the capture
// client generated: the proxy's peer-PID ancestry check cannot tell this
// plugin's requests apart from requests made by the harness's own
// subprocesses (a shell tool's child is a descendant too), and the echoed
// nonce is what does. Only this process was handed the value, the proxy
// validates and strips it before forwarding, and it must never be written
// anywhere else — not logged, not surfaced in the UI.
const GATEWAY_NONCE_HEADER = "x-tapes-gateway-nonce";

// Take the launch secret out of the environment before anything else runs.
// This is module scope, which opencode evaluates while loading plugins —
// before the first session exists and therefore before any tool can run.
// Subprocesses the harness later spawns inherit the harness's *current*
// environment, so reading the nonce once and deleting the variable here means
// those children never receive it; the value survives only in this module's
// closure. Deleted even when no gateway URL is set: an inert plugin must not
// leave the secret lying in the environment either. The URL and schema
// variables stay — they are addresses, not secrets, and other tooling may
// legitimately read them.
const nonce = process.env[GATEWAY_NONCE_ENV];
delete process.env[GATEWAY_NONCE_ENV];

const rawBaseUrl = process.env[GATEWAY_URL_ENV];

function normalizeBaseUrl(url: string): string {
  const trimmed = url.replace(/\/+$/, "");
  return trimmed.startsWith("http://") || trimmed.startsWith("https://") ? trimmed : `http://${trimmed}`;
}

function isCapturedProvider(provider: string): boolean {
  return (CAPTURED_PROVIDERS as readonly string[]).includes(provider);
}

// Is `candidate` an address on the capture proxy this plugin was pointed at?
//
// Gates the nonce and the envelope, so it is a security boundary and not a
// convenience: it decides whether a per-launch secret is put on a request. It
// must therefore compare URLs as URLs. A textual prefix test — the candidate
// string beginning with the gateway string — reads as if it means the same
// thing and does not: with a gateway at
// `https://gw.example`, such a test also accepts
// `https://gw.example.attacker.invalid`, an entirely different host that would
// then be handed the launch nonce and the session envelope. Registering a
// lookalike domain is cheap, and `options.baseURL` is user-editable config, so
// that is a live exfiltration path rather than a theoretical one.
//
// Both halves below are boundary comparisons on parsed components:
//
// * `origin` covers scheme, host, and port as one unit, so no host that merely
//   begins with the gateway's can pass. Comparing parsed origins rather than
//   splicing the string also avoids re-implementing authority parsing, which is
//   where this class of bug usually comes from. (An unparseable candidate, or
//   one with a non-HTTP scheme, gets the opaque origin `"null"`; the gateway URL
//   is normalised to http/https above, so its origin is never `"null"` and such
//   a candidate can never match.)
// * the path check keeps a gateway mounted on a sub-path from accepting a
//   sibling of it — `/capture` must not match `/capture-elsewhere` — while
//   still accepting the mount point itself and anything beneath it.
function isGatewayAddress(candidate: string, gatewayUrl: string): boolean {
  let url: URL;
  let gateway: URL;
  try {
    url = new URL(candidate);
    gateway = new URL(gatewayUrl);
  } catch {
    return false;
  }
  if (url.origin !== gateway.origin) {
    return false;
  }
  const mount = gateway.pathname.replace(/\/+$/, "");
  return url.pathname === mount || url.pathname.startsWith(`${mount}/`);
}

// A single export, and it is a function: opencode treats *every* export of a
// plugin module as a plugin factory and throws on one that is not callable.
export const TapesGateway = async ({ client }: { client?: any }) => {
  // No capture proxy configured, so this session is not being captured: leave
  // every provider on opencode's own endpoint and register no hooks.
  //
  // This is why there is no default address. The plugin installs into
  // opencode's global plugin directory, so it loads for *every* opencode
  // session on the machine, not only the ones launched under capture. A
  // built-in default would silently route ordinary sessions at whatever
  // happened to be listening on that port — and a capture proxy's port is
  // chosen per launch, so no fixed default could be right anyway.
  if (!rawBaseUrl) {
    return {};
  }

  const baseUrl = normalizeBaseUrl(rawBaseUrl);

  // opencode's provider adapters are AI SDK adapters: they append only the
  // *endpoint* segment to the configured base URL — `/messages` for Anthropic,
  // `/responses` or `/chat/completions` for OpenAI — and expect the `/v1`
  // component both upstream APIs put in front of it to already be part of the
  // base URL. The gateway URL contract is a bare proxy origin (the same value
  // every other harness's integration receives), so the adapter-shaped suffix
  // is this asset's to add: it is knowledge about opencode's HTTP client, not
  // about any particular proxy deployment. One suffix serves both providers,
  // because `/v1` is where both upstreams put their route root.
  const providerBaseUrl = `${baseUrl}/v1`;

  const activeSchema = process.env[GATEWAY_SCHEMA_ENV] ?? "unselected";

  // Warnings are best-effort and once per cause: a capture that cannot reach
  // the TUI (an SDK change, a headless run) must still capture.
  const warned = new Set<string>();
  const warn = (key: string, message: string) => {
    if (warned.has(key)) return;
    warned.add(key);
    try {
      void client?.tui?.showToast({ body: { message, variant: "warning" } });
    } catch {
      // The toast is advice, not capture; losing it costs nothing but the hint.
    }
  };

  return {
    // Runs once, after opencode has loaded its config and before providers are
    // built from it, which is the window this redirect needs: point each
    // captured provider's traffic at the proxy. Any other option the user set
    // on these providers survives — overwriting the base URL is the entire
    // point, and it is the only field touched.
    //
    // Providers this plugin did not route are left alone rather than pruned.
    // `crate::launch::OpenCodeRecipe` does prune them, and for a good reason
    // — an unrouted provider is a selectable route the proxy never sees — but
    // a recipe writes a private config for one launch, while this file is
    // installed globally and mutates the user's live config object. Deleting
    // a user's providers there would also have to unpick their persisted
    // model selection, and would do so for a session that may not even be
    // captured. The safe half of the same protection is below: a request
    // headed for an unrouted provider is warned about and never stamped.
    config: async (config: Record<string, any>) => {
      const providers = (config.provider ??= {});
      for (const name of CAPTURED_PROVIDERS) {
        const entry = (providers[name] ??= {});
        const options = (entry.options ??= {});
        options.baseURL = providerBaseUrl;
      }
    },

    // Runs per model call, which is where the session's identity is known:
    // stamp the envelope opencode carries on its own behalf. opencode is a
    // self-attributing harness — it publishes no PID-indexed session file, so
    // the capture client cannot recover this session's identity from a
    // peer-PID lookup, and what these headers carry is the only attribution
    // the turn will ever have. The nonce echo rides alongside it: without the
    // echo the proxy has no way to distinguish this plugin from a subprocess
    // of the harness forging an envelope.
    "chat.headers": async (
      input: { sessionID: string; model: { providerID: string }; provider?: { options?: Record<string, any> } },
      output: { headers: Record<string, string> },
    ) => {
      const providerID = input.model.providerID;
      if (!isCapturedProvider(providerID)) {
        warn(
          `uncaptured:${providerID}`,
          `capture covers opencode's Anthropic and OpenAI providers; ${providerID} uses its normal ` +
            "endpoint and will not be captured.",
        );
        return;
      }

      // The redirect must actually have stuck before anything is stamped. A
      // provider whose resolved base URL is not the proxy's — an auth loader
      // that swapped endpoints, a later config layer that won, a hand-edited
      // config naming a lookalike host — is sending this request somewhere
      // that is not the capture proxy, and the nonce must never travel there:
      // it is a secret shared with the proxy alone. See `isGatewayAddress`
      // for why this is a parsed-URL comparison and not a string prefix.
      const resolvedBaseUrl = input.provider?.options?.baseURL;
      if (typeof resolvedBaseUrl !== "string" || !isGatewayAddress(resolvedBaseUrl, baseUrl)) {
        warn(
          `unrouted:${providerID}`,
          `the ${providerID} provider is not routing through the capture proxy; this session's ` +
            `${providerID} turns will not be captured.`,
        );
        return;
      }

      if (isCapturedProvider(activeSchema) && providerID !== activeSchema) {
        warn(
          `schema:${providerID}`,
          `the capture proxy is routing the ${activeSchema} schema; the selected model's provider is ` +
            `${providerID}. Point ${GATEWAY_URL_ENV} at a proxy serving ${providerID}, or switch that ` +
            "proxy's active schema, if requests fail.",
        );
      }

      if (nonce) {
        output.headers[GATEWAY_NONCE_HEADER] = nonce;
      }
      Object.assign(output.headers, {
        "X-Tapes-Harness-Id": "opencode",
        "X-Tapes-Harness-Session-Id": input.sessionID,
      });
    },
  };
};
