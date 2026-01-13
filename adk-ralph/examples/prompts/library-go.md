# Rate Limiting Library - Go

Create a Go library for rate limiting called 'ratelimit'.

## Purpose

A flexible, high-performance rate limiting library supporting multiple algorithms and storage backends.

## Features

- **Algorithms**
  - Token bucket: Smooth rate limiting with burst capacity
  - Sliding window: Accurate request counting over time
  - Fixed window: Simple time-based windows
  - Leaky bucket: Constant output rate

- **Storage Backends**
  - In-memory: Fast, single-instance use
  - Redis: Distributed rate limiting
  - Interface for custom backends

- **Configuration**
  - Per-key rate limits
  - Dynamic limit adjustment
  - Configurable time windows
  - Burst allowance settings

- **Observability**
  - Metrics export (Prometheus format)
  - Remaining quota queries
  - Reset time information

## Technical Requirements

- Go 1.21+ with generics
- Context support for cancellation
- Thread-safe implementations
- Zero external dependencies for core
- Optional Redis dependency

## API Design

```go
// Core interface
type Limiter interface {
    Allow(ctx context.Context, key string) (bool, error)
    AllowN(ctx context.Context, key string, n int) (bool, error)
    Remaining(ctx context.Context, key string) (int, error)
    Reset(ctx context.Context, key string) error
}

// Configuration
type Config struct {
    Rate     int           // Requests per window
    Window   time.Duration // Time window
    Burst    int           // Burst capacity
}
```

## Testing

- Table-driven tests
- Benchmarks for each algorithm
- Race condition tests with -race
- Integration tests for Redis backend
- Example tests in documentation
