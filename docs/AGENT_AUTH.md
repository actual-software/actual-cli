# Access tokens for machines without a browser

`actual login` signs you in through the browser. That's fine for a person at a
keyboard. An Amp orb, a CI job or another machine nobody can open a browser on
needs a different way in, and that's what `actual auth create-token` and the
`ACTUAL_TOKEN` environment variable give it: you mint a **personal access
token** (PAT) from your own sign-in, once, and the machine uses it in place of
a sign-in.

## Two headless paths

- **`actual auth create-token`** mints a PAT from *your* `actual login`. It
  acts as you, in the organization you signed in to, with the scopes you ask
  for, for 90 days. The rest of this page describes it until the last section.
- **`actual mint-token`** needs no login at all. An agent holds a registered
  private key, signs a short-lived assertion with it, and trades that for an
  access token. See [Service-account keys](#service-account-keys).

## Create a token

On a computer signed in to the actual CLI:

```console
$ actual auth create-token --name amp-orbs --scopes adr:query,adr:review
actl_pat_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
✔ Created access token "amp-orbs"
  Organization:  Mockingbird Maps
  Expires:       2027-02-25T09:30:00Z

Add it in Amp Settings → Secrets & Env Vars → Personal as ACTUAL_TOKEN, then start a new thread (or run `amp orb restart-processes`). Don't paste it into a chat.
```

The token is printed **once**, to stdout, on its own line, so
`TOKEN=$(actual auth create-token …)` captures just the secret; everything
else goes to stderr. The CLI keeps no copy of it, so copy it now. `--name`
(1 to 64 printable ASCII characters) and `--scopes` (comma- or space-separated)
are required.

`create-token` sends `POST <api>/v1/oauth/tokens` with your sign-in's access
token as the bearer, where `<api>` is `--api-url`, else `ACTUAL_API_URL`, else
`https://api-service.api.prod.actual.ai`. It refuses to run while
`ACTUAL_TOKEN` is set: a token can't mint another.

Revoke a token in Actual AI → User Settings → Access tokens, which lists your
tokens in the organization you have open; switch organizations to see the
others. When an admin revokes **Actual CLI** in Integrations → Connected Apps,
every token in that organization stops working, and each person makes a new
one. `actual logout` doesn't revoke a token.

## Use it: `ACTUAL_TOKEN`

Set `ACTUAL_TOKEN` to the token, through a secret store (in Amp, a personal
secret), never in a prompt, a command line or a file a job logs. With it set,
every command that needs a sign-in uses the token instead of
`credentials.yaml`:

```console
$ actual whoami
✔ Signed in to Actual AI with ACTUAL_TOKEN (amp-orbs)
  Organization:  …
  Account:       you@example.com
  Member:        …
  Scopes:        adr:query adr:review
  Expires:       2027-02-25 09:30 UTC
```

How it works:

- **Who the token belongs to.** The CLI asks
  `GET <api>/v1/oauth/tokens/current` with the token as its bearer, and keeps
  the answer (organization, member, email, scopes, expiry) in
  `~/.actualai/actual/token-identity.yaml`, readable only by you. The file is
  keyed by the token's SHA-256, holds nothing from the token itself, and only
  the current token's entry. The answer is reused for 24 hours, and past that
  whenever Actual AI can't be reached, until the token expires. Each request
  gets 3 seconds; one that gets no usable answer (a timeout, a 404, a 5xx)
  isn't tried again for 5 minutes. A 401 is remembered for an hour.
  `actual whoami --check` always asks now.
- **Where it goes.** Only to Actual AI's api-service
  (`https://api-service.api.prod.actual.ai` or
  `https://api-service.api.staging.actual.ai`, scheme, host and port exactly,
  naming no user) or a loopback server. With `--api-url` or `ACTUAL_API_URL`
  pointing anywhere else the CLI refuses (exit 2): "ACTUAL_TOKEN works only
  with Actual AI. Unset ACTUAL_API_URL (or --api-url), or unset ACTUAL_TOKEN to
  use another server." On-prem deployments don't use PATs yet.
- **No renewal, no sign-out.** The token is never refreshed. `actual login` and
  `actual logout` leave it alone and say so; `logout` still removes a stored
  `actual login` session and the identity cache.
- **Never signed out by accident.** A token that can't be used is an error,
  not a signed-out run: `actual adr` reports it rather than falling back to
  the files in `docs/adr/`. Hooks still fail open.

| Situation | What you see | Exit |
| --- | --- | --- |
| Token expired or revoked (401) | "Your ACTUAL_TOKEN has expired or was revoked. …" with how to make a new one | 2 |
| Value isn't an `actl_pat_` token | "ACTUAL_TOKEN isn't an Actual access token. …" | 2 |
| Another server | "ACTUAL_TOKEN works only with Actual AI. …" | 2 |
| Actual AI unreachable, with no earlier answer for this token | "Couldn't check ACTUAL_TOKEN with Actual AI: …" or "Couldn't reach Actual AI to check ACTUAL_TOKEN. …" (an earlier answer keeps commands working) | 3 |
| Actual AI refuses one request but the token still works | "Actual AI refused this request, though your ACTUAL_TOKEN still works. …" | 2 |
| `--api-url` names another server than `ACTUAL_API_URL` | "With ACTUAL_TOKEN set, choose the server with ACTUAL_API_URL, not --api-url." | 2 |

## Two rules for agents

### 1. A dedicated token per agent or machine pool

Mint a separate token for each agent or pool of machines (`--name amp-orbs`),
never the human's interactive session. Every action is then attributable to
that token, and you revoke one without disturbing anything else.

### 2. The token never enters the model's context

Keep the token in an environment variable the model doesn't read. It must
never appear in a prompt or conversation, the shell history, a command-line
argument, a log line or an error report. An agent reads untrusted input, and a
prompt-injection payload in it can ask the agent to send any secret in its
context somewhere; a token that's never in the context can't be sent that way.

## Service-account keys

`create-token` still assumes a human logged in once. `mint-token` removes even
that. An agent holds a private key whose public half is registered server-side,
signs a short-lived assertion with it, and exchanges the assertion for an access
token. No browser, no prompt, nothing interactive. This is the
[RFC 7523](https://datatracker.ietf.org/doc/html/rfc7523) jwt-bearer grant, and
it's the path for a long-running unattended agent.

```console
$ export ACTUAL_SERVICE_ACCOUNT_KEY="$(cat service-account.pk8.pem)"
$ TOKEN=$(actual mint-token \
    --service-account-id 6f1d9c30-4b21-4f83-9f0c-2a7b5d8e1c44 \
    --kid sa-2026-08 \
    --scope adr:query --scope adr:review)
mint-token minting a token via jwt-bearer (ES256) as 6f1d9c30-…
✔ minted token (expires in 3600s; scope: adr:query adr:review)
```

The capture contract matches `create-token`: the access token is the **only**
thing written to stdout, so `TOKEN=$(actual mint-token …)` captures exactly the
token and nothing else. Status lines go to stderr, and the token never appears
there. Pass `--json` when you want the full response (`expires_in`, the granted
scope) as one machine-readable line on stdout instead.

`--service-account-id` (a UUID) and `--kid` are both required. The key id tells
the server which registered public key to verify against, so it has to name the
key you're actually signing with.

### Supplying the key

Two environment variables carry the key, and a key file wins when both are set:

| Environment variable | Purpose |
| --- | --- |
| `ACTUAL_SERVICE_ACCOUNT_KEY` | The PEM contents inline. Preferred with a secret manager: the key reaches the process without touching disk. |
| `ACTUAL_SERVICE_ACCOUNT_KEY_FILE` | Path to a PEM file. `--key <PATH>` is the flag form of the same thing. |

There's deliberately no flag that takes the key material itself. A
command-line argument is readable by every other process on the box and it
lands in shell history, which is the one place a signing key must never be.

Treat the private key as strictly more sensitive than a PAT. A leaked PAT is
revoked one token at a time. A leaked service-account key keeps minting fresh
tokens until someone rotates the registered public key.

### EC keys must be PKCS#8

An EC (P-256) key has to be PKCS#8, the PEM that opens with `BEGIN PRIVATE
KEY`. Nothing exotic, just the newer of the two encodings. The older SEC1 encoding (`BEGIN EC PRIVATE KEY`), which is what
`openssl ecparam -genkey` writes by default, is refused before anything is
signed:

```console
$ actual mint-token --key sec1.pem --service-account-id <uuid> --kid <kid>
Error: SEC1 EC private key ('BEGIN EC PRIVATE KEY'); PKCS#8 is required
Fix: openssl pkcs8 -topk8 -nocrypt -in <key.pem> -out <key.pk8.pem>
```

Convert once and point at the result:

```bash
openssl pkcs8 -topk8 -nocrypt -in sec1.pem -out service-account.pk8.pem
```

The conversion re-encodes the same key pair, so the registered public key is
untouched and `--kid` stays as it was. You get the same refusal and the same
fix whether the algorithm was inferred from the key or you passed `--alg es256`
yourself. RSA keys carry no equivalent restriction: PKCS#1 (`BEGIN RSA PRIVATE
KEY`) and PKCS#8 both load.

The refusal is a deliberate choice rather than a gap to work around. The
signer this CLI pins
cannot load a SEC1 key at all, so accepting one would only move the failure
later, to signing time, where the error no longer names the key format. Failing
at the door with the conversion command attached is the more useful answer.

### Algorithm, lifetime, and audience

`--alg` is inferred from the key, with EC signing ES256 and RSA signing RS256.
Pass it explicitly only to pin what you expect. Just `rs256` and `es256` are
accepted; `HS*` and `none` are refused outright, so the client cannot be talked
into emitting a symmetric or unsigned assertion.

`--assertion-ttl-seconds` bounds the assertion rather than the token the server
returns. It defaults to 60 and is capped at 300. That short window plus a fresh
`jti` on every call is what makes a captured assertion close to useless: the
server anti-replays on the `jti`, and the assertion expires in a minute anyway.

`--issuer` sets the authorization server base URL, resolved from the flag, then
`ACTUAL_AUTH_URL`, then the production default. The token endpoint is
`<issuer>/api/oauth/token`. `--aud` overrides the assertion audience, which
otherwise follows the issuer origin. A non-HTTPS, non-loopback issuer is
rejected before any assertion leaves the process.

`--scope` is repeatable and asks for a subset of the principal's grant. Omit it
and the server mints the principal's full whitelist.

### In CI

> **Not yet supported.** `ACTUAL_TOKEN` takes only `actl_pat_` tokens from
> `actual auth create-token`, so the CLI refuses an access token from
> `mint-token` placed there. The recipe below shows the intended flow.

```yaml
- name: Mint a service-account token
  env:
    ACTUAL_SERVICE_ACCOUNT_KEY: ${{ secrets.ACTUAL_SERVICE_ACCOUNT_KEY }}
    ACTUAL_SERVICE_ACCOUNT_ID: ${{ secrets.ACTUAL_SERVICE_ACCOUNT_ID }}
    ACTUAL_SERVICE_ACCOUNT_KID: ${{ vars.ACTUAL_SERVICE_ACCOUNT_KID }}
  run: |
    TOKEN=$(actual mint-token --scope adr:query)
    echo "::add-mask::$TOKEN"
    echo "ACTUAL_TOKEN=$TOKEN" >> "$GITHUB_ENV"
```

The three required inputs (the id, the key id, and the key) all have
environment-variable forms, so a CI step passes arguments only for what it
wants to vary. Mint per job instead of storing a token:
the assertion costs nothing, the token is short-lived, and the only long-lived
secret is the key sitting in the secret store. Mask the minted token before it
can reach a log, as the `add-mask` line does, because a token minted at runtime
is not one of the secrets the platform already knows to redact.
