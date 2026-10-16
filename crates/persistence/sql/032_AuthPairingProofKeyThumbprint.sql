-- Port of upstream/apps/server/src/persistence/Migrations/032_AuthPairingProofKeyThumbprint.ts
ALTER TABLE auth_pairing_links
      ADD COLUMN proof_key_thumbprint TEXT;
