# Certificates

## `isrg-root-x1.der`

The one trust anchor `src/tls.rs` verifies the API's certificate chain against, in DER because that
is the encoding `MbedTLS` can parse without copying the certificate into RAM and because a DER file
is binary: `.gitattributes` says so, so Git stores it byte for byte and the gate's line-ending and
spelling steps skip it rather than trying to read base64 as prose.

It is `ISRG Root X1`, the self-signed root of the **Internet Security Research Group**, which is
what Let's Encrypt — and therefore the deployed Cloudflare Worker behind `*.workers.dev` — serves
certificates under. Measured on 2026-10-07 against `cfpoc.andresmoschini.workers.dev`: the chain the
server presents is

```text
CN=andresmoschini.workers.dev   ← Let's Encrypt "YE2"  ← ISRG "Root YE"  ← ISRG Root X2  ← ISRG Root X1
```

The server sends the three intermediates; only the last one is a trust anchor, and a root never
travels with a handshake, so this file is the one that has to be on the chip.

To refresh or replace it, take it from the authority rather than from the handshake — a certificate
fetched from the peer it is meant to vouch for is worth nothing — and re-measure the chain with
`openssl s_client -showcerts`:

```sh
openssl s_client -connect cfpoc.andresmoschini.workers.dev:443 \
  -servername cfpoc.andresmoschini.workers.dev -showcerts </dev/null
```

**A pin is a promise, and this one has a shelf life.** If the API is ever moved behind a different
authority — a different CA, or a different CDN whose certificates do not chain to ISRG — every
handshake fails with a verification error rather than silently trusting whatever is offered, and
this file is what has to change. See the note on certificate dates in [`src/tls.rs`](../src/tls.rs):
the dates in this certificate are **not** checked, so what is actually being promised is "the chain
leads to ISRG", not "the chain leads to ISRG and is current".
