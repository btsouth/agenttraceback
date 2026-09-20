# Command Code fixture profile

Command Code's public product does not currently document a stable local semantic
database or log format. The adapter therefore detects the `command-code`,
`commandcode`, or `cmdcode` executable and common application directories, reports
semantic import as unavailable, and directs recording through:

```text
agenttraceback run --agent command-code -- command-code
```

No guessed parser is shipped. Add a semantic parser only when a sanitized,
versioned local format can be validated.
