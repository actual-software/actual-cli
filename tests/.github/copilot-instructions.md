# GitHub Copilot Instructions for actual-software/actual-cli

## ADR 1: Adopt Protocol-Based Public API Design for External Integration

Implement a protocol-based public API design pattern that defines explicit contracts for external interactions. This architectural decision establishes formal protocols/interfaces that serve as the public API surface, separating the contract from implementation details. The pattern is consistently applied across integration test scenarios, ensuring that external consumers interact through well-defined protocol boundaries rather than directly with internal implementations.