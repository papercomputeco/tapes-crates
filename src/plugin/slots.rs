//! Substituting consumer values into a crate-owned asset template.
//!
//! Two kinds of asset in this module are templates rather than fixed files —
//! the Codex hook-plugin manifests ([`super::codex_app`]) and pi's gateway
//! extension ([`super::pi`]) — and they share one substitution rule because
//! the property they need is the same one: a rendered consumer value must
//! land in the output as *data*, never as syntax.
//!
//! Both templates spell a slot as a quoted placeholder, `"__TAPES_NAME__"`,
//! and [`render_slots`] replaces the placeholder *including its quotes* with a
//! complete escaped string literal. JSON and TypeScript agree on string
//! literal syntax closely enough for one escaper to serve both: JSON's grammar
//! (RFC 8259 §7) is a subset of ECMAScript's, so a valid JSON string literal
//! is a valid TypeScript string literal with the same value. That is why
//! [`string_literal`] is written once here instead of once per asset kind.

/// Replace every quoted slot occurrence with its escaped value, in one pass
/// over the template.
///
/// Substitution targets `"__SLOT__"` including its quotes and emits a
/// complete string literal, so escaping cannot be forgotten and a slot can
/// never be half-replaced inside a larger value. Single-pass is load-bearing,
/// not a micro-optimisation: only *template* text is ever scanned for slots,
/// and substituted values go straight to the output. A sequential per-slot
/// `replace` re-scans earlier insertions, so a value that merely *contains*
/// another slot's placeholder — pathological but consumer-controlled — would
/// itself get substituted. Here such a value passes through verbatim
/// (escaped), like every other value byte.
pub(super) fn render_slots(template: &str, slots: &[(&str, &str)]) -> String {
    let mut rendered = String::with_capacity(template.len());
    let mut rest = template;
    loop {
        // The earliest quoted slot in the remaining *template* text wins;
        // everything before it is emitted untouched.
        let next = slots
            .iter()
            .filter_map(|(slot, value)| {
                let quoted = format!("\"{slot}\"");
                rest.find(&quoted).map(|at| (at, quoted.len(), *value))
            })
            .min_by_key(|(at, ..)| *at);
        let Some((at, slot_len, value)) = next else {
            rendered.push_str(rest);
            return rendered;
        };
        rendered.push_str(&rest[..at]);
        rendered.push_str(&string_literal(value));
        rest = &rest[at + slot_len..];
    }
}

/// `value` as a complete double-quoted string literal, quotes included.
///
/// Hand-rolled rather than `serde_json::to_string` because that API returns a
/// `Result` this crate would have to pretend can fail; for a `&str` it cannot,
/// and the escaping rules (RFC 8259 §7: `"` and `\` escaped, control
/// characters as `\u00XX`) are small enough to state directly.
///
/// The output is deliberately conservative for the TypeScript use: a backtick
/// or a `${` in a value is *not* escaped, and does not need to be, because a
/// double-quoted literal is not a template literal — inside these quotes both
/// are ordinary characters. What would break out is a bare `"`, a trailing
/// `\`, or a raw newline, and those are exactly what is escaped here.
pub(super) fn string_literal(value: &str) -> String {
    let mut literal = String::with_capacity(value.len() + 2);
    literal.push('"');
    for character in value.chars() {
        match character {
            '"' => literal.push_str("\\\""),
            '\\' => literal.push_str("\\\\"),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\t' => literal.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                literal.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => literal.push(other),
        }
    }
    literal.push('"');
    literal
}
