-- Port of upstream/apps/server/src/persistence/Migrations/021_AuthSessionClientMetadata.ts
ALTER TABLE auth_pairing_links
      ADD COLUMN label TEXT;
ALTER TABLE auth_sessions
      ADD COLUMN client_label TEXT;
ALTER TABLE auth_sessions
      ADD COLUMN client_ip_address TEXT;
ALTER TABLE auth_sessions
      ADD COLUMN client_user_agent TEXT;
ALTER TABLE auth_sessions
      ADD COLUMN client_device_type TEXT NOT NULL DEFAULT 'unknown';
ALTER TABLE auth_sessions
      ADD COLUMN client_os TEXT;
ALTER TABLE auth_sessions
      ADD COLUMN client_browser TEXT;
