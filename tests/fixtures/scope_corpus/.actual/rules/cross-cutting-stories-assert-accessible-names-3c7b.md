# Adopt Accessibility Assertions in Component Stories: Stories Assert Accessible Names

These rules are ALWAYS ACTIVE for all component stories and their interaction tests in web/components/, covering keyboard navigation, focus order, and screen reader labelling of interactive elements.

### Rules

- **R-STORYB-001** MUST: Every interactive element in a story is queried by role and accessible name.
- **R-STORYB-002** MUST: Stories assert focus order for composite widgets.
- **R-STORYB-003** SHOULD: Decorative images are asserted to be hidden from the accessibility tree.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "stories" web/components/ --include="*.tsx"
grep -rE "stories|assert" web/.storybook/
test -d web/components/ && echo "governed tree present"
```

**Accept when:**
- Every interactive element in a story is queried by role and accessible name
- Stories assert focus order for composite widgets
- Decorative images are asserted to be hidden from the accessibility tree

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
