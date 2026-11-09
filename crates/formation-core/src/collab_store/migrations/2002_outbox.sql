-- The asker's outbox (FR-016): calls waiting for a teammate's Mac, kept so
-- formationd retries them after a restart. Version 2002, in B's range 2000-2999,
-- applied after 2001. No transaction or PRAGMA statements, as in 2001.
--
-- The entry is JSON `OutboxEntry` text: it is only ever read whole, and its
-- payload (a `PeerAsk` or `PeerRequestCreate`) follows the peer protocol, so a
-- protocol change needs no migration here. The key columns are the asker-chosen
-- ID (`OutboxKey`) that `put` replaces by and `remove` deletes by. Entries load
-- in insertion order (rowid); an upsert keeps the rowid, so a replaced entry
-- keeps its place in its teammate's queue.
CREATE TABLE outbox_entry (
    key_kind TEXT NOT NULL CHECK (key_kind IN ('question', 'request')),
    key_id TEXT NOT NULL,
    member TEXT NOT NULL,
    entry TEXT NOT NULL,       -- JSON OutboxEntry
    PRIMARY KEY (key_kind, key_id)
) STRICT;
