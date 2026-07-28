-- Creates the role and database Flux owns.
--
-- Run once, as a superuser, before the first `fluxd` start:
--
--     psql "postgres://postgres@localhost/postgres" -f deploy/sql/bootstrap.sql
--
-- Schema itself is not created here. `fluxd` applies its own sqlx migrations on
-- every start, so there is exactly one definition of the schema and it travels
-- with the binary that expects it.

\set ON_ERROR_STOP on

-- The application password. Override at the command line for anything that is
-- not a throwaway development box:
--
--     psql ... -v flux_password="'a-real-password'" -f deploy/sql/bootstrap.sql
\if :{?flux_password}
\else
  \set flux_password '''flux'''
\endif

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'flux') THEN
        EXECUTE format('CREATE ROLE flux LOGIN PASSWORD %L', :flux_password);
        RAISE NOTICE 'created role flux';
    ELSE
        RAISE NOTICE 'role flux already exists, leaving its password alone';
    END IF;
END
$$;

-- CREATE DATABASE cannot run inside a transaction or a DO block, so it is
-- guarded with \gexec instead: the SELECT produces the statement only when the
-- database is absent, and \gexec runs whatever the query returned.
SELECT 'CREATE DATABASE flux OWNER flux'
WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'flux')
\gexec

-- Postgres 15 and later revoke CREATE on the public schema from everyone but the
-- database owner. `flux` owns the database, so it already has what the migrations
-- need; this is here for the case where the database was created by hand under a
-- different owner.
\connect flux
GRANT ALL ON SCHEMA public TO flux;
