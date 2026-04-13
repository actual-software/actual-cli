The system requires a stable, well-defined interface for external consumers and integration testing. As the codebase evolved, there was a need to establish clear boundaries between internal implementation details and the public-facing API surface. Integration tests across multiple scenarios (no tailor, sync modes, smoke tests) revealed the necessity for a consistent protocol-based approach to ensure API stability, testability, and backward compatibility for external clients.

## Policies
- Implement a protocol-based public API design pattern that defines explicit contracts for external interactions. This architectural decision establishes formal protocols/interfaces that serve as the public API surface, separating the contract from implementation details. The pattern is consistently applied across integration test scenarios, ensuring that external consumers interact through well-defined protocol boundaries rather than directly with internal implementations.

## Instructions
- Positive: Clear separation between public API contracts and internal implementation, enabling independent evolution of both
- Positive: Enhanced testability through protocol-based integration tests that validate external-facing behavior
- Positive: Improved backward compatibility guarantees for external API consumers
- Positive: Better documentation surface as protocols explicitly define the public contract
- Positive: Reduced coupling between external consumers and internal implementation details
- Trade-off: Additional abstraction layer introduces slight complexity overhead
- Trade-off: Requires discipline to maintain protocol boundaries and avoid leaking implementation details
- Trade-off: May require more upfront design effort to define appropriate protocol boundaries
- Limitation: Changes to public protocols require careful versioning and migration strategies