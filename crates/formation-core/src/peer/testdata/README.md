# Tailscale WhoIs fixtures (TASK-007)

Sanitized `apitype.WhoIsResponse` bodies, the JSON that both `tailscale whois --json <addr>` and the LocalAPI's `GET /localapi/v0/whois?addr=<addr>` return (Tailscale 1.104.2, Mac App Store build). Every login, name, ID, key and address is made up; the field names, nesting and omitted fields follow real responses.

- `whois-user.json`: a peer Mac that belongs to a user. Like a real response, it has no `Tags` key.
- `whois-tagged.json`: a tagged node. Written from Tailscale's documentation and source rather than observed on the team's tailnet: tagged nodes have no user, and WhoIs names the placeholder owner `tagged-devices`.

- `status-peers.json`: an `ipnstate.Status`, the JSON that both `tailscale status --json` and the LocalAPI's `GET /localapi/v0/status` (with peers) return, cut down to the fields the peer directory reads and a few around them. `Peer` is keyed by node key, and each entry's `ID` is the node's `StableID`, the value WhoIs reports as `Node.StableID`. It holds this Mac, a teammate's Mac that lists its IPv6 address first, a tagged server, and a node with only an IPv6 address. Written from Tailscale's `ipnstate` source in the same shape as the WhoIs fixtures, not captured from the team's tailnet.

`formation_core::peer` tests read them from here. B's `stub-whois` (TASK-042) can copy them into `testkit/` when it lands.
