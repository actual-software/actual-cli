# Adopt Accessibility Assertions in Component Stories: Stories Cover Loading And Error States

These rules are ALWAYS ACTIVE for all component stories and their interaction tests in web/components/, covering keyboard navigation, focus order, and screen reader labelling of interactive elements.

### Rules

- **R-STORYB-021** MUST: Every data-driven component has a loading story and an error story.
- **R-STORYB-022** MUST: Error stories assert that the message is announced politely.
- **R-STORYB-023** SHOULD: Empty states are distinguished from loading states in the story name.

### Verify

```bash
# Confirm the governed modules exist and carry the expected shape
grep -r "stories" web/components/ --include="*.tsx"
grep -rE "stories|cover" web/.storybook/
test -d web/components/ && echo "governed tree present"
```

**Accept when:**
- Every data-driven component has a loading story and an error story
- Error stories assert that the message is announced politely
- Empty states are distinguished from loading states in the story name

<enforcement>
Claude Code MUST NOT skip or defer verification of these rules.
</enforcement>
