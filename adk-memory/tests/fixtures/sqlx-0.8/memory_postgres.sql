--
-- PostgreSQL database dump
--


-- Dumped from database version 17.7
-- Dumped by pg_dump version 17.7

SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET transaction_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;

SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: _adk_memory_migrations; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public._adk_memory_migrations (
    version bigint NOT NULL,
    description text NOT NULL,
    applied_at text NOT NULL
);


--
-- Name: memory_entries; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.memory_entries (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    app_name text NOT NULL,
    user_id text NOT NULL,
    session_id text NOT NULL,
    content jsonb NOT NULL,
    author text NOT NULL,
    "timestamp" timestamp with time zone NOT NULL,
    embedding public.vector(1536),
    search_text tsvector,
    project_id text
);


--
-- Data for Name: _adk_memory_migrations; Type: TABLE DATA; Schema: public; Owner: -
--

INSERT INTO public._adk_memory_migrations VALUES (1, 'create memory_entries table with vector and tsvector columns', '2026-10-14T10:40:47.088978+00:00');
INSERT INTO public._adk_memory_migrations VALUES (2, 'add project_id column and composite index', '2026-10-14T10:40:47.090005+00:00');


--
-- Data for Name: memory_entries; Type: TABLE DATA; Schema: public; Owner: -
--

INSERT INTO public.memory_entries VALUES ('4c86141d-5f1a-49ac-a55c-c634a8409381', 'upgrade_app', 'upgrade_user', 'upgrade_session', '{"role": "user", "parts": [{"text": "O''Brien prefers the harbour at high tide"}]}', 'user', '2026-01-02 03:04:05+00', NULL, '''at'':6 ''brien'':2 ''harbour'':5 ''high'':7 ''o'':1 ''prefers'':3 ''the'':4 ''tide'':8', NULL);


--
-- Name: _adk_memory_migrations _adk_memory_migrations_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public._adk_memory_migrations
    ADD CONSTRAINT _adk_memory_migrations_pkey PRIMARY KEY (version);


--
-- Name: memory_entries memory_entries_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.memory_entries
    ADD CONSTRAINT memory_entries_pkey PRIMARY KEY (id);


--
-- Name: idx_memory_app_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_memory_app_user ON public.memory_entries USING btree (app_name, user_id);


--
-- Name: idx_memory_app_user_project; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_memory_app_user_project ON public.memory_entries USING btree (app_name, user_id, project_id);


--
-- Name: idx_memory_embedding; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_memory_embedding ON public.memory_entries USING hnsw (embedding public.vector_cosine_ops) WITH (m='16', ef_construction='64');


--
-- Name: idx_memory_search_text; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_memory_search_text ON public.memory_entries USING gin (search_text);


--
-- PostgreSQL database dump complete
--


