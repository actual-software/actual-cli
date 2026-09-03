# Adopt Accessibility Assertions in Component Stories: Interaction Tests Use Play Functions

These rules are ALWAYS ACTIVE for all component stories and their interaction tests in web/components/, covering keyboard navigation, focus order, and screen reader labelling of interactive elements.

### Rules

- **R-STORYB-011** MUST: User interaction is driven through a play function rather than a separate test file.
- **R-STORYB-012** MUST: Assertions run against the canvas element rather than the document body.
- **R-STORYB-013** SHOULD: Play functions await every user event.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "interaction" web/components/ --include="*.tsx"
grep -rE "interaction|tests" web/.storybook/
test -d web/components/ && echo "governed tree present"
```

**Accept when:**
- User interaction is driven through a play function rather than a separate test file
- Assertions run against the canvas element rather than the document body
- Play functions await every user event

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
