# Real-Time Chat Application - TypeScript

Create a real-time chat application with TypeScript.

## Purpose

A full-stack chat application with real-time messaging, user presence, and room management.

## Features

- **Authentication**
  - User registration and login
  - Session management with refresh tokens
  - OAuth integration (Google, GitHub)
  - Profile management with avatars

- **Chat Rooms**
  - Create public and private rooms
  - Join/leave rooms
  - Room member management
  - Room settings and moderation

- **Messaging**
  - Real-time message delivery
  - Typing indicators
  - Read receipts
  - Message editing and deletion
  - File/image attachments
  - Emoji reactions

- **User Experience**
  - Online/offline presence
  - Unread message counts
  - Message search
  - Notification preferences

## Technical Requirements

### Backend
- Express.js with TypeScript
- Socket.io for WebSocket communication
- PostgreSQL with Prisma ORM
- Redis for session storage and pub/sub
- JWT for authentication

### Frontend
- React 18 with TypeScript
- Tailwind CSS for styling
- React Query for data fetching
- Zustand for state management
- Socket.io-client for real-time

### Infrastructure
- Docker Compose for local development
- Environment-based configuration

## Testing

- Vitest for unit tests
- Playwright for E2E tests
- MSW for API mocking
