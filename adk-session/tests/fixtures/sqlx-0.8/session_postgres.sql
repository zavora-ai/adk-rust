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
-- Name: _adk_session_migrations; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public._adk_session_migrations (
    version bigint NOT NULL,
    description text NOT NULL,
    applied_at text NOT NULL
);


--
-- Name: app_states; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.app_states (
    app_name text NOT NULL,
    state jsonb DEFAULT '{}'::jsonb NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: events; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.events (
    id text NOT NULL,
    app_name text NOT NULL,
    user_id text NOT NULL,
    session_id text NOT NULL,
    invocation_id text NOT NULL,
    branch text NOT NULL,
    author text NOT NULL,
    "timestamp" timestamp with time zone NOT NULL,
    llm_response jsonb NOT NULL,
    actions jsonb NOT NULL,
    long_running_tool_ids jsonb NOT NULL
);


--
-- Name: sessions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.sessions (
    app_name text NOT NULL,
    user_id text NOT NULL,
    session_id text NOT NULL,
    state jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: user_states; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_states (
    app_name text NOT NULL,
    user_id text NOT NULL,
    state jsonb DEFAULT '{}'::jsonb NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Data for Name: _adk_session_migrations; Type: TABLE DATA; Schema: public; Owner: -
--

INSERT INTO public._adk_session_migrations VALUES (1, 'create initial session tables', '2026-10-14T10:40:47.069433+00:00');


--
-- Data for Name: app_states; Type: TABLE DATA; Schema: public; Owner: -
--

INSERT INTO public.app_states VALUES ('upgrade_app', '{"plan": "team"}', '2026-10-14 10:40:47.072887+00');


--
-- Data for Name: events; Type: TABLE DATA; Schema: public; Owner: -
--

INSERT INTO public.events VALUES ('event-0', 'upgrade_app', 'upgrade_user', 'upgrade_session', 'invocation-1', '', 'user', '2026-10-14 10:40:47.071337+00', '{"content": {"role": "user", "parts": [{"text": "When is high tide?"}]}, "partial": false, "error_code": null, "interrupted": false, "error_message": null, "finish_reason": null, "turn_complete": false, "usage_metadata": null}', '{"escalate": false, "state_delta": {"turns": 1}, "artifact_delta": {}, "transfer_to_agent": null, "skip_summarization": false}', '[]');
INSERT INTO public.events VALUES ('event-1', 'upgrade_app', 'upgrade_user', 'upgrade_session', 'invocation-1', '', 'assistant', '2026-10-14 10:40:47.072887+00', '{"content": {"role": "assistant", "parts": [{"text": "High tide is at 06:12."}]}, "partial": false, "error_code": null, "interrupted": false, "error_message": null, "finish_reason": null, "turn_complete": false, "usage_metadata": null}', '{"escalate": false, "state_delta": {"turns": 2}, "artifact_delta": {}, "transfer_to_agent": null, "skip_summarization": false}', '[]');


--
-- Data for Name: sessions; Type: TABLE DATA; Schema: public; Owner: -
--

INSERT INTO public.sessions VALUES ('upgrade_app', 'upgrade_user', 'upgrade_session', '{"topic": "tides", "turns": 2, "app:plan": "team", "user:name": "O''Brien"}', '2026-10-14 10:40:47.070208+00', '2026-10-14 10:40:47.072887+00');


--
-- Data for Name: user_states; Type: TABLE DATA; Schema: public; Owner: -
--

INSERT INTO public.user_states VALUES ('upgrade_app', 'upgrade_user', '{"name": "O''Brien"}', '2026-10-14 10:40:47.072887+00');


--
-- Name: _adk_session_migrations _adk_session_migrations_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public._adk_session_migrations
    ADD CONSTRAINT _adk_session_migrations_pkey PRIMARY KEY (version);


--
-- Name: app_states app_states_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.app_states
    ADD CONSTRAINT app_states_pkey PRIMARY KEY (app_name);


--
-- Name: events events_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.events
    ADD CONSTRAINT events_pkey PRIMARY KEY (id, app_name, user_id, session_id);


--
-- Name: sessions sessions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.sessions
    ADD CONSTRAINT sessions_pkey PRIMARY KEY (app_name, user_id, session_id);


--
-- Name: user_states user_states_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_states
    ADD CONSTRAINT user_states_pkey PRIMARY KEY (app_name, user_id);


--
-- Name: idx_events_session_ts; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_events_session_ts ON public.events USING btree (session_id, "timestamp");


--
-- Name: idx_sessions_app_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idx_sessions_app_user ON public.sessions USING btree (app_name, user_id);


--
-- Name: events events_app_name_user_id_session_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.events
    ADD CONSTRAINT events_app_name_user_id_session_id_fkey FOREIGN KEY (app_name, user_id, session_id) REFERENCES public.sessions(app_name, user_id, session_id) ON DELETE CASCADE;


--
-- PostgreSQL database dump complete
--


