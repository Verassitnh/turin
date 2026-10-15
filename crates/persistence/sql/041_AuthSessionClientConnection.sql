ALTER TABLE auth_sessions
      ADD COLUMN client_surface TEXT;
ALTER TABLE auth_sessions
      ADD COLUMN client_app_version TEXT;
