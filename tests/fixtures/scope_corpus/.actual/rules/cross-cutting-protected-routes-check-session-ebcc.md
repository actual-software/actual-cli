# Adopt Server-Side Session Handling for Next.js Authentication: Protected Routes Check Session

These rules are ALWAYS ACTIVE for all server-side authentication operations in the Next.js application, including user sign-in, registration, session refresh, and demo user authentication flows.

### Rules

- **R-NEXTAU-021** MUST: Every route segment under the authenticated group resolves the session before rendering.
- **R-NEXTAU-022** MUST: A missing session redirects rather than rendering an empty shell.
- **R-NEXTAU-023** SHOULD: Route handlers re-check authorisation rather than trusting the layout.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "protected" web/lib/auth.ts --include="*.ts"
grep -rE "protected|routes" web/app/
test -d web/lib/auth.ts && echo "governed tree present"
```

**Accept when:**
- Every route segment under the authenticated group resolves the session before rendering
- A missing session redirects rather than rendering an empty shell
- Route handlers re-check authorisation rather than trusting the layout

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
