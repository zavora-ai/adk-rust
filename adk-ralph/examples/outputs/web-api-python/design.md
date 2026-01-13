# System Design: bookstore-api

## Architecture Overview

The bookstore API follows a layered architecture with FastAPI handling HTTP requests, SQLAlchemy for database operations, and Pydantic for data validation.

```mermaid
flowchart TB
    subgraph Client
        HTTP[HTTP Requests]
    end
    
    subgraph API["API Layer"]
        FAST[FastAPI]
        AUTH[Auth Middleware]
        VALID[Pydantic Validation]
    end
    
    subgraph Service["Service Layer"]
        BOOK[Book Service]
        USER[User Service]
        ORDER[Order Service]
        CART[Cart Service]
    end
    
    subgraph Data["Data Layer"]
        REPO[Repositories]
        ORM[SQLAlchemy ORM]
        DB[(PostgreSQL)]
    end
    
    HTTP --> FAST
    FAST --> AUTH
    AUTH --> VALID
    VALID --> BOOK
    VALID --> USER
    VALID --> ORDER
    VALID --> CART
    BOOK --> REPO
    USER --> REPO
    ORDER --> REPO
    CART --> REPO
    REPO --> ORM
    ORM --> DB
```

## Component Diagram

```mermaid
classDiagram
    class Book {
        +int id
        +str title
        +str author
        +str isbn
        +Decimal price
        +int stock
        +str category
        +datetime created_at
    }
    
    class User {
        +int id
        +str email
        +str hashed_password
        +Role role
        +bool is_active
        +datetime created_at
    }
    
    class Order {
        +int id
        +int user_id
        +OrderStatus status
        +Decimal total
        +datetime created_at
    }
    
    class OrderItem {
        +int id
        +int order_id
        +int book_id
        +int quantity
        +Decimal price
    }
    
    class CartItem {
        +int id
        +int user_id
        +int book_id
        +int quantity
    }
    
    Order --> User
    Order --> OrderItem
    OrderItem --> Book
    CartItem --> User
    CartItem --> Book
```

## File Structure

```
bookstore-api/
├── requirements.txt
├── alembic.ini
├── main.py                 # FastAPI app entry point
├── config.py               # Configuration settings
├── database.py             # Database connection
├── models/
│   ├── __init__.py
│   ├── book.py             # Book SQLAlchemy model
│   ├── user.py             # User SQLAlchemy model
│   ├── order.py            # Order SQLAlchemy model
│   └── cart.py             # Cart SQLAlchemy model
├── schemas/
│   ├── __init__.py
│   ├── book.py             # Book Pydantic schemas
│   ├── user.py             # User Pydantic schemas
│   ├── order.py            # Order Pydantic schemas
│   └── cart.py             # Cart Pydantic schemas
├── routes/
│   ├── __init__.py
│   ├── books.py            # Book endpoints
│   ├── auth.py             # Authentication endpoints
│   ├── users.py            # User management endpoints
│   ├── orders.py           # Order endpoints
│   └── cart.py             # Cart endpoints
├── services/
│   ├── __init__.py
│   ├── book_service.py
│   ├── user_service.py
│   ├── order_service.py
│   └── cart_service.py
├── auth/
│   ├── __init__.py
│   ├── jwt.py              # JWT utilities
│   ├── password.py         # Password hashing
│   └── dependencies.py     # Auth dependencies
├── alembic/
│   └── versions/           # Database migrations
└── tests/
    ├── conftest.py         # Test fixtures
    ├── test_books.py
    ├── test_auth.py
    ├── test_orders.py
    └── test_cart.py
```

## Technology Stack

- **Framework**: FastAPI 0.100+
- **ORM**: SQLAlchemy 2.0
- **Validation**: Pydantic 2.0
- **Database**: SQLite (dev) / PostgreSQL (prod)
- **Migrations**: Alembic
- **Auth**: python-jose (JWT), passlib (bcrypt)
- **Testing**: pytest, pytest-asyncio, httpx

## API Endpoints

### Books
- `GET /books` - List books (paginated, filterable)
- `GET /books/{id}` - Get book details
- `POST /books` - Create book (staff+)
- `PUT /books/{id}` - Update book (staff+)
- `DELETE /books/{id}` - Delete book (admin)
- `POST /books/import` - Bulk import (staff+)

### Authentication
- `POST /auth/register` - Register new user
- `POST /auth/login` - Login, get JWT
- `POST /auth/refresh` - Refresh token
- `POST /auth/password-reset` - Request password reset

### Users
- `GET /users/me` - Get current user
- `PUT /users/me` - Update current user
- `GET /users` - List users (admin)
- `PUT /users/{id}/role` - Update user role (admin)

### Cart
- `GET /cart` - Get cart contents
- `POST /cart/items` - Add item to cart
- `PUT /cart/items/{id}` - Update quantity
- `DELETE /cart/items/{id}` - Remove item
- `DELETE /cart` - Clear cart

### Orders
- `GET /orders` - List user's orders
- `GET /orders/{id}` - Get order details
- `POST /orders` - Create order from cart

## Authentication Flow

```mermaid
sequenceDiagram
    participant C as Client
    participant A as API
    participant DB as Database
    
    C->>A: POST /auth/login {email, password}
    A->>DB: Find user by email
    DB-->>A: User record
    A->>A: Verify password hash
    A->>A: Generate JWT
    A-->>C: {access_token, token_type}
    
    C->>A: GET /orders (Authorization: Bearer token)
    A->>A: Validate JWT
    A->>A: Extract user_id, role
    A->>DB: Query orders for user
    DB-->>A: Orders
    A-->>C: Orders list
```

## Error Handling

- Use HTTPException for API errors
- Return RFC 7807 problem details format
- Log errors with correlation IDs
- Validate all inputs with Pydantic

## Testing Strategy

### Unit Tests
- Service layer logic
- JWT token generation/validation
- Password hashing

### Integration Tests
- Full endpoint testing with TestClient
- Database operations with test database
- Authentication flows

### Fixtures
- Factory Boy for test data
- pytest fixtures for database setup
