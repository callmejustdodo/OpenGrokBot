# Tailscale WhoIs fixtures (TASK-007)

Sanitized `apitype.WhoIsResponse` bodies, the JSON that both `tailscale whois --json <addr>` and the LocalAPI's `GET /localapi/v0/whois?addr=<addr>` return (Tailscale 1.104.2, Mac App Store build). Every login, name, ID, key and address is made up; the field names, nesting and omitted fields follow real responses.

- `whois-user.json`: a peer Mac that belongs to a user. Like a real response, it has no `Tags` key.
- `whois-tagged.json`: a tagged node. Written from Tailscale's documentation and source rather than observed on the team's tailnet: tagged nodes have no user, and WhoIs names the placeholder owner `tagged-devices`.

`formation_core::peer` tests read them from here. B's `stub-whois` (TASK-042) can copy them into `testkit/` when it lands.
