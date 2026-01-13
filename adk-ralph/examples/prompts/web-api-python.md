# Bookstore REST API - Python

Create a REST API for a bookstore in Python using FastAPI.

## Purpose

A backend service for managing a bookstore's inventory, customer orders, and user authentication.

## Features

- **Book Management**
  - CRUD operations for books (title, author, ISBN, price, stock quantity)
  - Search books by title, author, or ISBN
  - Filter by category, price range, availability
  - Bulk import books from CSV

- **User Management**
  - User registration with email verification
  - JWT-based authentication
  - Role-based access (admin, staff, customer)
  - Password reset functionality

- **Order Processing**
  - Shopping cart management
  - Order creation and tracking
  - Order history for users
  - Stock validation on order

## Technical Requirements

- FastAPI for the web framework
- SQLAlchemy with SQLite (dev) / PostgreSQL (prod)
- Pydantic for request/response validation
- Alembic for database migrations
- python-jose for JWT handling
- Passlib for password hashing

## API Design

- RESTful endpoints following OpenAPI 3.0
- Pagination for list endpoints
- Proper HTTP status codes
- Comprehensive error responses

## Testing

- pytest for unit and integration tests
- pytest-asyncio for async tests
- Factory Boy for test fixtures
- Coverage target: 80%
