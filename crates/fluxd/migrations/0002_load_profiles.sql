-- Stateful (L4-7) load profiles, and the tests that drive them.
--
-- A test names flows, profiles, or both. Flows are stateless frame templates;
-- profiles are connection-level loads. They are separate columns rather than one
-- polymorphic list because they are programmed through different engine calls
-- and a test that mixed them would need two engine instances in different modes.

ALTER TABLE tests
    ADD COLUMN profile_ids UUID[] NOT NULL DEFAULT '{}';

-- Previously every test had at least one flow. Now a test may instead have at
-- least one profile, and this is the constraint that keeps "neither" out.
ALTER TABLE tests
    ADD CONSTRAINT tests_reference_something
    CHECK (cardinality(flow_ids) > 0 OR cardinality(profile_ids) > 0);

-- Mirrors the flows index: used to refuse deleting a profile a test depends on.
CREATE INDEX tests_profile_ids_idx ON tests USING GIN (profile_ids);
CREATE INDEX tests_flow_ids_idx ON tests USING GIN (flow_ids);

-- ---------------------------------------------------------------------------
-- Settings introduced with milestone 4
-- ---------------------------------------------------------------------------

-- TLS is off until a certificate is uploaded. The paths are where fluxd writes
-- what it is given; the private key never leaves the appliance.
INSERT INTO settings (key, value) VALUES
    ('tls', '{"enabled": false, "certPath": null, "keyPath": null, "subject": null, "notAfter": null}'::JSONB)
ON CONFLICT (key) DO NOTHING;

-- How long results and time series are kept. The janitor enforces it.
INSERT INTO settings (key, value) VALUES
    ('retention', '{"runDays": 90, "seriesDays": 30}'::JSONB)
ON CONFLICT (key) DO NOTHING;

INSERT INTO settings (key, value) VALUES
    ('appliance', '{"hostname": null, "location": null, "contact": null}'::JSONB)
ON CONFLICT (key) DO NOTHING;
