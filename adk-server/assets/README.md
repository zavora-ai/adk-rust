# ADK Runtime UI assets

This directory contains the production build embedded by `adk-server`. The
maintained React and TypeScript source lives in [`../webui`](../webui); these
files are checked in so crate consumers do not need Node.js.

Regenerate the assets after changing the frontend:

```bash
cd adk-server/webui
npm ci
npm run verify
```

The Vite build writes directly to `adk-server/assets/webui`. Commit source and
generated assets together. The interface is owned by ADK-Rust and uses the ADK
Studio Next design language; it is not copied from the Google ADK Web UI.
