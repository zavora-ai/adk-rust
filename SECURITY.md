# Security Policy

## Supported Versions

The following versions of ADK-Rust are currently supported with security updates:

| Version | Supported          |
| ------- | ------------------ |
| 0.2.x   | :white_check_mark: |
| 0.1.x   | :x:                |

We recommend always using the latest version to benefit from security patches and improvements.

## Reporting a Vulnerability

We take security vulnerabilities seriously. If you discover a security issue, please report it responsibly.

### How to Report

**Please do NOT report security vulnerabilities through public GitHub issues.**

Instead, report vulnerabilities via one of these methods:

1. **Email**: Send details to [security@zavora.ai](mailto:security@zavora.ai)
2. **GitHub Security Advisories**: Use [GitHub's private vulnerability reporting](https://github.com/zavora-ai/adk-rust/security/advisories/new)

### What to Include

When reporting a vulnerability, please include:

- Description of the vulnerability
- Steps to reproduce the issue
- Potential impact assessment
- Any suggested fixes (if available)
- Your contact information for follow-up

### Response Timeline

- **Initial Response**: Within 48 hours of receiving your report
- **Status Update**: Within 7 days with our assessment
- **Resolution Target**: Critical vulnerabilities within 30 days, others within 90 days

### What to Expect

1. **Acknowledgment**: We'll confirm receipt of your report
2. **Investigation**: Our team will investigate and validate the issue
3. **Communication**: We'll keep you informed of our progress
4. **Credit**: With your permission, we'll acknowledge your contribution in the security advisory

### Disclosure Policy

- We follow coordinated disclosure practices
- We request a 90-day disclosure window to address vulnerabilities
- Security advisories will be published after fixes are available

## Security Best Practices

When using ADK-Rust in your applications:

- Keep dependencies up to date
- Store API keys and secrets securely (use environment variables, not code)
- Validate and sanitize all inputs to agents
- Use guardrails for input/output validation
- Review agent outputs before taking automated actions
- Implement proper authentication for server deployments

## Scope

This security policy applies to:

- All ADK-Rust crates published on crates.io
- The official repository at github.com/zavora-ai/adk-rust
- Official documentation and examples

Third-party integrations and forks are outside the scope of this policy.

## Contact

For security-related questions that aren't vulnerabilities, you can reach us at [security@zavora.ai](mailto:security@zavora.ai).
