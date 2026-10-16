-- Port of upstream/apps/server/src/persistence/Migrations/022_AuthSessionLastConnectedAt.ts
ALTER TABLE auth_sessions
      ADD COLUMN last_connected_at TEXT;
