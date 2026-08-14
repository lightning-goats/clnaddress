# Lightning Goats `clnaddress` Contract

This fork remains based on [`daywalker90/clnaddress`](https://github.com/daywalker90/clnaddress), while adding a small, explicit contract required by the standalone Lightning Goats service.

The contract is versioned because `lightning-goatsd` uses the server-generated CLN invoice label as its trusted payment classifier.

## Invoice label contract

Invoices created for a configured Lightning Address user use:

```text
clnaddress:v1:<user>:<uuid>
```

Examples:

```text
clnaddress:v1:herd:550e8400-e29b-41d4-a716-446655440000
clnaddress:v1:donate:7e4299a6-3ae4-4b71-a79e-8dd8ce84ef50
```

Invoices created through the generic `/lnurlp` endpoint use:

```text
clnaddress:v1:__direct__:<uuid>
```

`__direct__` is deliberately not a valid configured user because canonical usernames must start and end with an ASCII letter or digit.

`lightning-goatsd` credits only labels matching the configured user, currently:

```text
clnaddress:v1:herd:<valid UUID>
```

Descriptions, LNURL comments, Nostr events, and other payer-controlled values are not payment identity.

## Canonical usernames

Configured users must:

- contain 1–64 ASCII characters;
- be lowercase;
- contain only `a-z`, `0-9`, `.`, `_`, and `-`;
- start and end with a letter or digit;
- not contain consecutive dots.

Examples accepted:

```text
herd
sat
donate
goat-1
goat_2
goat.3
```

Examples rejected:

```text
Herd
-herd
herd-
herd..goats
herd/goats
herd:goats
```

Persisted `users.json` entries are validated with the same rules at startup. The plugin refuses to start with an invalid persisted user rather than creating ambiguous public routes or invoice labels.

## Per-address policy

The original positional API remains supported:

```bash
lightning-cli clnaddress-adduser herd false "Feed the Lightning Goats"
```

Richer configuration should use named parameters:

```bash
lightning-cli clnaddress-adduser \
  user=herd \
  description="Feed the Lightning Goats" \
  min_sendable_msat=1000 \
  max_sendable_msat=100000000 \
  comment_allowed=250 \
  nostr_enabled=true
```

Supported fields:

| Field | Meaning |
| --- | --- |
| `user` | Canonical Lightning Address username. Required. |
| `is_email` | Advertise `text/email` instead of `text/identifier`. |
| `description` | Address-specific LNURL description. |
| `min_sendable_msat` | Address-specific minimum. Falls back to the global plugin option. |
| `max_sendable_msat` | Address-specific maximum. Falls back to the global plugin option. |
| `comment_allowed` | LUD-12 comment character limit, 1–512. Omit to disable comments. |
| `nostr_enabled` | Enable/disable NIP-57 for this address. Defaults to enabled when a global Zap signer is configured, preserving upstream behavior. |

Unknown named fields are rejected so misspelled security/policy settings cannot be silently ignored.

The effective per-address minimum must not exceed the effective maximum.

## LNURL comments

When `comment_allowed` is configured, the initial LNURL-pay response advertises `commentAllowed` and the callback accepts a `comment` query parameter up to that many Unicode characters.

Comments are validated but are **not appended to the CLN invoice description**. LNURL wallets validate the BOLT11 description hash against the metadata returned before the callback, so changing the description based on the later comment would invalidate the LNURL payment contract.

Phase 1 Lightning Goats does not use LNURL comments as payment identity or feeder input.

## Per-address Nostr policy

A configured global Zap signer remains required for NIP-57 support.

Individual addresses may then disable Zap support:

```bash
lightning-cli clnaddress-adduser user=donate nostr_enabled=false
```

For such an address:

- `allowsNostr` is `false`;
- `nostrPubkey` is omitted;
- a callback containing a Nostr Zap request is rejected.

An address with `nostr_enabled=true`, or with the field omitted for backward compatibility, advertises Zap support when the global signer exists.

## Zap receipt signer secret

The upstream inline option remains available for compatibility:

```text
clnaddress-nostr-privkey=<secret>
```

Production Lightning Goats deployments should instead use:

```text
clnaddress-nostr-privkey-file=/path/to/credential
```

Rules for the file option:

- inline and file-based secret sources are mutually exclusive;
- the path must be a regular file;
- symbolic links are rejected;
- on Unix, group/other permissions are rejected (`0600` or stricter is required);
- surrounding whitespace/newlines are trimmed before parsing;
- an empty or invalid key disables startup with an explicit error.

The Zap receipt key is a dedicated purpose-specific identity. It must not be the main Lightning Goats project Nostr identity. The main project identity remains behind the separate `nak` NIP-46 bunker.

## Upgrade rule

Any future change to the label grammar or attribution semantics requires either:

1. preserving the `v1` grammar exactly; or
2. introducing a new version such as `clnaddress:v2:...` and updating `lightning-goatsd` deliberately.

Never silently reinterpret `v1` labels.
