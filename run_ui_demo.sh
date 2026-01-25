#!/bin/bash

# ADK-UI Demo Runner
# Starts the Rust server and React client

set -e

echo "🚀 Starting ADK-UI Demo..."
echo ""

# Load .env file if it exists
if [ -f .env ]; then
    set -a
    source .env
    set +a
    echo "✅ Loaded .env file"
fi

# Check for API key
if [ -z "$GOOGLE_API_KEY" ] && [ -z "$GEMINI_API_KEY" ]; then
    echo "❌ Error: GOOGLE_API_KEY or GEMINI_API_KEY must be set in .env"
    exit 1
fi

# Check if node_modules exists
if [ ! -d "examples/ui_react_client/node_modules" ]; then
    echo "📦 Installing React client dependencies..."
    cd examples/ui_react_client
    npm install
    cd ../..
fi

echo "✅ Starting Rust server on http://localhost:8080"
echo "✅ Starting React client on http://localhost:5173"
echo ""
echo "🌐 Open http://localhost:5173 in your browser"
echo ""
echo "Press Ctrl+C to stop both servers"
echo ""

# Start server in background
cargo run --example ui_server &
SERVER_PID=$!

# Wait for server to start
sleep 3

# Start React client
cd examples/ui_react_client
npm run dev &
CLIENT_PID=$!

# Wait for both processes
wait $SERVER_PID $CLIENT_PID
