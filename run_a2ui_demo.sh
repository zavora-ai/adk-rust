#!/bin/bash

# Load environment variables
if [ -f .env ]; then
    export $(cat .env | grep -v '^#' | xargs)
fi

echo "Starting A2UI Demo with React UI..."
echo ""
echo "This will start:"
echo "  1. UI Server on http://localhost:8080"
echo "  2. React Client on http://localhost:5173"
echo ""

# Start UI server in background
echo "Starting UI server..."
cargo run --example ui_server &
SERVER_PID=$!

# Wait for server to start
sleep 3

# Start React client
echo "Starting React client..."
cd examples/ui_react_client
npm run dev &
CLIENT_PID=$!

echo ""
echo "✅ Services started!"
echo "   UI Server: http://localhost:8080"
echo "   React Client: http://localhost:5173"
echo ""
echo "Press Ctrl+C to stop all services"

# Wait for Ctrl+C
trap "kill $SERVER_PID $CLIENT_PID 2>/dev/null; exit" INT
wait
