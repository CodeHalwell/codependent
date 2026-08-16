-- Artifact retrieval is authorized from kernel-derived peer credentials.
-- Existing daemon-internal artifacts remain NULL and are adopted by the
-- daemon uid at authorization time, matching other pre-ownership stores.
ALTER TABLE artifacts ADD COLUMN owner_uid INTEGER;
