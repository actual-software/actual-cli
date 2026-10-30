<!-- Describe the change and why it's needed. -->

## Summary

## Telemetry & privacy checklist

Complete this section for any change that adds, removes, or alters a telemetry
field or identifier, or how identifiers are hashed, peppered, or mapped. Delete
the section if the PR touches no telemetry.

- [ ] Every new/changed telemetry field is listed in `PRIVACY.md` with an
      explicit privacy class (**anonymous**, **pseudonymous**, or
      **first-party identity-linked**). Pseudonymous means the field carries no
      account identity, but a stable key such as an installation id or a
      repository hash can link events to each other, to a known repository, or,
      once that installation has sent a logged-in event, to an account.
- [ ] No field Actual can attribute to an account (a deterministic identity
      hash, a mapping key, or any logged-in identifier) is described as
      "anonymous" or as carrying "no PII". Hashing alone is not anonymization.
- [ ] For any first-party identity-linked field, `PRIVACY.md` documents who can
      attribute it, the purpose, and the retention/deletion behavior.
- [ ] Wording is coordinated with the paired backend (`sprintreview`) PR.
- [ ] Privacy/product approval obtained (required for identity-field or
      privacy-classification changes).
