# ADK Web UI Assets

This directory contains the pre-built frontend assets for the ADK Web UI.

## Source

These assets are compiled from the Angular application located in:
```
adk-go/cmd/launcher/web/webui/
```

## Contents

- `index.html` - Main HTML entry point
- `*.js` - Compiled JavaScript bundles
- `*.css` - Stylesheets
- `assets/` - Images, fonts, and configuration
- `adk_favicon.svg` - ADK favicon

## Updating Assets

To update the Web UI (when upstream changes):

1. Navigate to the adk-go repository
2. Build the Web UI:
   ```bash
   cd cmd/launcher/web/webui
   npm install
   npm run build
   ```
3. Copy the built assets:
   ```bash
   cp -r distr/* ../../../../adk-rust/adk-server/assets/webui/
   ```

## License

Copyright 2025 Google LLC

Licensed under the Apache License, Version 2.0.
See the main LICENSE file in the repository root.
