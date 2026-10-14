PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE graph_checkpoints (
                id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL,
                state TEXT NOT NULL,
                step INTEGER NOT NULL,
                pending_nodes TEXT NOT NULL,
                metadata TEXT,
                created_at TEXT NOT NULL,
                cleared_interrupt TEXT,
                attempts TEXT,
                child_ledger TEXT
            );
INSERT INTO graph_checkpoints VALUES('upgrade_checkpoint','upgrade_thread','{"note":"O''Brien","count":2}',3,'["review"]','{}','2026-01-02T03:04:05+00:00',NULL,'{}','{}');
CREATE INDEX idx_graph_checkpoints_thread
            ON graph_checkpoints(thread_id, created_at DESC);
COMMIT;
