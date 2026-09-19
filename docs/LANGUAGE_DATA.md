# Language attribution

## Why GraphQL?

GH Archive events say *what happened* and *on which repo*, not a reliable
language mix. Until September 2025 some payloads included a primary-language
field; [from October 2025 GitHub stripped it](https://github.blog/changelog/2025-08-08-upcoming-changes-to-github-events-api-payloads/).

This project always resolves languages via GraphQL
(`repo_language_loader` → `data/repo-languages/…`).

## Primary-only vs all languages

Through **2025-09**, ratings use one language per repo (event primary, or a
single-language stub). From **2025-10**, ratings split each repo’s activity
across **all** languages by byte share:

```
rating[L] += event_count × (size_L / total_size)
```

Same filtered events, primary-only vs proportional (2025-10 / 2026-01 PR):

| | Primary-only | All languages |
|---|---|---|
| Top 5 order | Stable | Same |
| CSS, SCSS, Shell, Makefile | Under-counted (often not primary) | Get fractional credit |
| CSS score vs proportional | ~35% | 100% |
| Go, Rust, Java, C#, PHP | Slightly higher | Slight dilution |

**We keep all-languages (proportional).** Top rankings barely move; secondary
languages would otherwise disappear. The CSS step at 2025-10 is that method
change — document it, don’t paper over it with primary-only.
