# Product Requirements Document

## Project Overview

**Project Name**: bookstore-api
**Language**: Python
**Type**: REST API

A backend service for managing a bookstore's inventory, customer orders, and user authentication using FastAPI with SQLAlchemy.

## Glossary

- **Book**: A product in the bookstore with title, author, ISBN, price, and stock
- **User**: A registered account with email, password, and role
- **Order**: A customer purchase containing line items
- **Cart**: Temporary storage for items before checkout
- **JWT**: JSON Web Token for authentication
- **Role**: User permission level (admin, staff, customer)

## User Stories

### US-001: Book CRUD Operations

**Priority**: 1
**Status**: pending

**Description**: As a staff member, I want to manage books in the inventory, so that I can keep the catalog up to date.

**Acceptance Criteria**:
1. WHEN a staff member sends POST /books with valid data, THE system SHALL create a new book
2. WHEN a user sends GET /books, THE system SHALL return a paginated list of books
3. WHEN a user sends GET /books/{id}, THE system SHALL return the book details
4. WHEN a staff member sends PUT /books/{id}, THE system SHALL update the book
5. WHEN an admin sends DELETE /books/{id}, THE system SHALL remove the book
6. THE system SHALL validate ISBN format (ISBN-10 or ISBN-13)
7. THE system SHALL prevent duplicate ISBNs

### US-002: Book Search

**Priority**: 1
**Status**: pending

**Description**: As a customer, I want to search for books, so that I can find what I'm looking for.

**Acceptance Criteria**:
1. WHEN a user sends GET /books?search=query, THE system SHALL search title and author
2. WHEN a user sends GET /books?category=fiction, THE system SHALL filter by category
3. WHEN a user sends GET /books?min_price=10&max_price=50, THE system SHALL filter by price range
4. WHEN a user sends GET /books?in_stock=true, THE system SHALL show only available books
5. THE system SHALL support combining multiple filters

### US-003: User Registration

**Priority**: 1
**Status**: pending

**Description**: As a visitor, I want to create an account, so that I can make purchases.

**Acceptance Criteria**:
1. WHEN a visitor sends POST /auth/register with valid data, THE system SHALL create a user
2. THE system SHALL hash passwords before storing
3. THE system SHALL validate email format
4. THE system SHALL prevent duplicate email addresses
5. THE system SHALL assign 'customer' role by default
6. THE system SHALL return a JWT token upon successful registration

### US-004: User Authentication

**Priority**: 1
**Status**: pending

**Description**: As a user, I want to log in, so that I can access my account.

**Acceptance Criteria**:
1. WHEN a user sends POST /auth/login with valid credentials, THE system SHALL return a JWT token
2. WHEN credentials are invalid, THE system SHALL return 401 Unauthorized
3. THE system SHALL include user role in the JWT payload
4. THE system SHALL set appropriate token expiration
5. WHEN a user sends POST /auth/refresh, THE system SHALL issue a new token

### US-005: Role-Based Access

**Priority**: 2
**Status**: pending

**Description**: As an admin, I want to control access to operations, so that only authorized users can perform them.

**Acceptance Criteria**:
1. THE system SHALL allow customers to view books and manage their own orders
2. THE system SHALL allow staff to manage books and view all orders
3. THE system SHALL allow admins to manage users and perform all operations
4. WHEN an unauthorized user attempts a restricted operation, THE system SHALL return 403 Forbidden

### US-006: Shopping Cart

**Priority**: 2
**Status**: pending

**Description**: As a customer, I want to manage a shopping cart, so that I can prepare my order.

**Acceptance Criteria**:
1. WHEN a customer sends POST /cart/items, THE system SHALL add an item to their cart
2. WHEN a customer sends GET /cart, THE system SHALL return their cart contents
3. WHEN a customer sends DELETE /cart/items/{id}, THE system SHALL remove the item
4. WHEN a customer sends PUT /cart/items/{id}, THE system SHALL update the quantity
5. THE system SHALL validate stock availability when adding items

### US-007: Order Processing

**Priority**: 1
**Status**: pending

**Description**: As a customer, I want to place orders, so that I can purchase books.

**Acceptance Criteria**:
1. WHEN a customer sends POST /orders, THE system SHALL create an order from their cart
2. THE system SHALL validate stock for all items before creating the order
3. THE system SHALL reduce stock quantities upon order creation
4. THE system SHALL clear the cart after successful order creation
5. WHEN a customer sends GET /orders, THE system SHALL return their order history
6. WHEN a customer sends GET /orders/{id}, THE system SHALL return order details

### US-008: Bulk Import

**Priority**: 3
**Status**: pending

**Description**: As a staff member, I want to import books from CSV, so that I can add many books at once.

**Acceptance Criteria**:
1. WHEN a staff member sends POST /books/import with a CSV file, THE system SHALL create books
2. THE system SHALL validate each row before importing
3. THE system SHALL report success and failure counts
4. THE system SHALL skip rows with duplicate ISBNs
