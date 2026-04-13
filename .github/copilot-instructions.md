# GitHub Copilot Instructions

## ADR 1: Adopt Modular Library Architecture for Test and Benchmark Infrastructure

1. Implement a modular library architecture where core functionality is organized into reusable library modules that can be consumed by benchmarks, tests, and test utilities. This decision establishes a clear separation between production code (src/), test utilities (src/testutil.rs), integration tests (tests/), and performance benchmarks (benches/), with each component importing and utilizing core library modules as needed. The architecture promotes code reuse by centralizing common test utilities in dedicated modules that can be shared across different testing contexts.

---

## ADR 2: Adopt Secure Secrets Management in CI/CD Pipeline

1. Implement a dedicated secrets management system integrated into the CI/CD pipeline that separates secret storage from code. This includes using environment variables, encrypted secret stores, or dedicated secrets management services (such as HashiCorp Vault, AWS Secrets Manager, or CI platform-native solutions) to inject credentials at runtime rather than hardcoding them in configuration files or source code. The pattern is consistently applied across CLI commands and testing infrastructure to ensure uniform security practices.