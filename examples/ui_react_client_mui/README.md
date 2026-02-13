# MUI Client (Enterprise/Themed)

This is an alternative frontend client for ADK UI examples, built with **React** and **Material UI (MUI)**. It provides a polished, enterprise-grade interface for interacting with ADK agents.

## Quick Start

```bash
# Start the backend server (e.g., ui_server or ralph_autonomous_agent)
# See root-level instructions for specific backend examples

# In another terminal, start this client
cd examples/ui_react_client_mui
npm install
npm run dev
```

The application will be available at [http://localhost:3001](http://localhost:3001).

## Architecture

This client connects to ADK agents via SSE (Server-Sent Events) and renders dynamic UI components generated through `render_*` tool calls.

```
┌─────────────────┐     SSE      ┌──────────────┐
│  React Client   │◄────────────│  ADK Agent   │
│   (MUI + Vite)  │             │  (Rust)      │
│                 │────POST────►│              │
└─────────────────┘  /api/run   └──────────────┘
         │                              │
         ▼                              ▼
   MUI Renderer                  LlmAgent + UiToolset
```

## Features

*   **Material Design**: Uses MUI components for a consistent and professional look (replacing Tailwind CSS from the base client).
*   **Themed**: Supports light/dark mode (configured in `App.tsx`).
*   **Multi-Agent Support**: Connects to various backend examples (UI Demo, Support, Appointments, etc.).
*   **Dynamic Components**: Renders Forms, Cards, Alerts, Tables, Charts, Progress bars, and Layouts dynamically from backend instructions.

## Key Files

- `src/adk-ui-renderer/types.ts` - TypeScript types matching Rust schema
- `src/adk-ui-renderer/Renderer.tsx` - Component renderer adapted for MUI
- `src/App.tsx` - Main app with SSE connection and theme configuration

## Configuration

The client connects to backend services defined in `src/App.tsx`. You can configure the API URL via `.env` file (see `.env.example`).

## Customization

The renderer uses Material UI theming. Modify `App.tsx` to customize the theme palette or `Renderer.tsx` to add new component types.

## Production Build

```bash
npm run build
# Output in dist/
```
