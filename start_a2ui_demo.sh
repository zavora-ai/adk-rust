#!/bin/bash

cd /Users/jameskaranja/Developer/projects/adk-rust

# Load environment
if [ -f .env ]; then
    export $(cat .env | grep -v '^#' | xargs)
fi

echo "=== Starting A2UI Demo ==="
echo ""
echo "Services:"
echo "  • UI Server: http://localhost:8080"
echo "  • React Client: http://localhost:5173"
echo ""
echo "Starting UI server..."

# Start server in background
cargo run --example ui_server > /tmp/ui_server.log 2>&1 &
SERVER_PID=$!

# Wait for server
sleep 3

echo "✅ UI Server started (PID: $SERVER_PID)"
echo ""
echo "Starting React client..."

# Start React client
cd examples/ui_react_client
npm run dev > /tmp/react_client.log 2>&1 &
CLIENT_PID=$!

sleep 2

echo "✅ React Client started (PID: $CLIENT_PID)"
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "🎉 A2UI Demo Ready!"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "Open in browser: http://localhost:5173"
echo ""
echo "Try asking:"
echo "  • Create a welcome screen"
echo "  • Show me a dashboard"
echo "  • Create a login form"
echo ""
echo "Logs:"
echo "  • Server: tail -f /tmp/ui_server.log"
echo "  • Client: tail -f /tmp/react_client.log"
echo ""
echo "Press Ctrl+C to stop all services"
echo ""

# Cleanup function
cleanup() {
    echo ""
    echo "Stopping services..."
    kill $SERVER_PID $CLIENT_PID 2>/dev/null
    echo "✅ Services stopped"
    exit 0
}

trap cleanup INT TERM

# Keep script running
wait
