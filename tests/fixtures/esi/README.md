# ESI fixtures

Responses recorded from the live ESI (`X-Compatibility-Date: 2026-08-18`)
for use with wiremock. Tests never call real ESI.

| File | Request | Recorded |
| --- | --- | --- |
| `characters_affiliation.json` | `POST /characters/affiliation` with `[196379789, 443630591, 1887431749, 406944591]` (Chribba, The Mittani, gigX, mynnna) | 2026-09-24 |
| `universe_ids.json` | `POST /universe/ids` with `["Goonswarm Federation", "GoonWaffe", "Pandemic Horde", "No Such Alliance Exists 123"]` | 2026-09-24 |
| `universe_names.json` | `POST /universe/names` with `[159826257, 1695357456, 1164409536, 98133756]` | 2026-09-24 |
| `status.json` | `GET /status` | 2026-09-24 |
