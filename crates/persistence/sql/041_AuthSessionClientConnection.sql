-- Port of upstream/apps/server/src/persistence/Migrations/041_AuthSessionClientConnection.ts
ALTER TABLE auth_sessions
      ADD COLUMN client_surface TEXT;
ALTER TABLE auth_sessions
      ADD COLUMN client_app_version TEXT;
