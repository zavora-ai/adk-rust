# Order Processing Microservice - Java

Create a Spring Boot microservice for order processing.

## Purpose

A production-ready microservice handling order lifecycle management with event-driven architecture.

## Features

- **Order Management**
  - Create orders with line items
  - Update order details
  - Cancel orders (with validation)
  - Query orders with filtering and pagination

- **Order Workflow**
  - Status transitions: PENDING → CONFIRMED → PROCESSING → SHIPPED → DELIVERED
  - Validation rules for each transition
  - Automatic status updates from external events

- **Integration**
  - Payment service integration (mock for development)
  - Inventory service integration
  - Notification service for status updates

- **Events**
  - Publish order events to Kafka
  - Event types: OrderCreated, OrderConfirmed, OrderShipped, etc.
  - Idempotent event handling

## Technical Requirements

- Spring Boot 3.x with Java 21
- Spring Data JPA with PostgreSQL
- Spring Kafka for event publishing
- Spring Security with OAuth2
- Flyway for database migrations
- OpenAPI 3.0 documentation

## API Design

- RESTful endpoints
- HATEOAS links for navigation
- Proper error responses with problem details (RFC 7807)
- Request validation with Bean Validation

## Observability

- Spring Actuator for health checks
- Micrometer metrics
- Distributed tracing with Sleuth
- Structured logging with correlation IDs

## Testing

- JUnit 5 for unit tests
- Testcontainers for integration tests
- WireMock for external service mocking
- Contract tests with Spring Cloud Contract
- Coverage target: 80%
