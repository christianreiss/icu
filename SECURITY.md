# Security policy

## Reporting a vulnerability

Please report security problems privately through GitHub:
**[Report a vulnerability](https://github.com/christianreiss/icu/security/advisories/new)**
(Security tab → “Report a vulnerability”). Do not open a public issue.

Helpful details: the affected version, client or server, the platform, and
steps or a proof of concept. English or German, both are fine.

## Supported versions

Only the [latest release](https://github.com/christianreiss/icu/releases/latest)
receives fixes. The Windows client updates itself after you confirm; Linux
packages update through your package manager.

## Scope

The server (`icu-server`), the desktop client, the wire protocol
([`docs/reference/protocol.md`](docs/reference/protocol.md)), the packages and
the signed update feed on icu.uggs.io.

ICU's trust model is described in the README: every server runs its own CA,
and the server relays media it can read. That the server operator can read
media is by design, not a vulnerability.
