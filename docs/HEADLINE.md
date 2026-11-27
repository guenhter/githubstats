# Monthly headline

The site shows a curated **monthly headline** under the page title. Headlines are **not** computed in the browser. An agent (or human) writes each month’s blurb during the monthly data update and stores it in [`data/stats/headlines.json`](../data/stats/headlines.json). `index.html` only looks up the entry for the current “as of” month (chart zoom end) and renders it.

## File format

`data/stats/headlines.json` is a single JSON object: keys are `YYYY-MM`, values are the **body only** (no month label prefix) — plain text, one short paragraph:

```json
{
  "2026-10": "TypeScript kept pulling away at the top…"
}
```

On the page this becomes:

```
October Headline: TypeScript kept pulling away at the top…
```

- Add **one new key for every published month** — no gaps. The UI hides the headline only if a key is missing (legacy / error); new months must always have an entry.
- One editorial blurb per month. The story is always based on **pull-request count** (`pr-count`), not the other site metrics.

## When to write it

During every monthly update (after ratings for month `M` exist):

1. Inspect the top **20** languages on **pull-request count** only (`data/stats/language-ratings-all-pr-count.jsonl`) across **1 month** (`M` vs `M−1`), **1 year** (`M` vs ~`M−12`), and **5 years** (`M` vs ~`M−60`), including the shape of each trend. Do not switch the headline story to pushes, issues, stars, or active-repos.
2. Follow the guidelines below and write a short paragraph.
3. Append `"YYYY-MM": "…"` to `data/stats/headlines.json`.
4. Regenerate the README screenshot (`./scripts/capture_readme_screenshot.sh`) so the hero includes the latest headline.

## Guidelines for agents

These are judgment calls, not a formula. Prefer a readable editorial take over “largest `|Δ pp|` wins.”

1. **Look at three horizons: 1 month, 1 year, 5 years.** Compare ranks and shares for `M` vs `M−1`, vs ~`M−12`, and vs ~`M−60`, and whether the latest month continues, stalls, or reverses those arcs. A quiet month MoM can still be interesting if it sits on a strong 1y or 5y rise or decline; a loud MoM move matters less if it fights a flat or opposite longer trend. The **5-year** view is especially for structural arcs a single year can hide—languages that have been **steadily losing** (or steadily gaining) share for years, former leaders fading, long climbs to dominance. A −2 pp year might look ordinary until the 5y chart shows a continuous grind downward. Use that lens when it changes the story; do not pad every blurb with three deltas. Ignore tiny noise; a 0.1 pp wobble in the long tail is not a headline.
2. **Weigh absolute and relative change** (on each horizon you cite). A +2 pp move on a ~25% language (e.g. TypeScript) is a smaller *relative* shift than +1 pp on a ~5% language. Prefer the story that is remarkable in context — large languages need bigger absolute moves to feel like news; smaller top-20 languages can headline on a sharper proportional jump or rank shake-up. Neither absolute Δ pp nor relative % change alone decides the headline.
3. **Watch top-20 entries and exits — but distrust one-month spikes.** Languages that **enter** or **leave** the top 20 between `M−1` and `M` can be more interesting than reshuffles among incumbents. Prefer newcomers/dropouts that look durable (especially a return after absence, or a long-time member leaving). If something looks suspicious — e.g. a language that was nowhere near the top 20 suddenly appearing, or an outsized jump with no prior trend — **do not headline it yet**. Wait a month or two to see whether it sticks; we do not want spike-driven headlines. In the meantime, write the month around steadier stories (or call it quiet).
4. **Ask what a skimming reader would care about.** Leadership changes, a clear surge or slide among the top languages, top-20 churn, a shake-up in the top 5, or a month that clearly continues (or breaks) a 1y/5y pattern usually beat a slightly larger one-off move further down the list.
5. **Lead with one story; support briefly.** Open with the main takeaway, then add a little context from the horizons that matter (last month, past year, past five years, a runner-up, or an entry/exit). Do not catalog the whole top 20 or dump a table of Δ pp figures across every window. When the month is **not** a flatline, write only about what moved — do **not** pad with absences (“no newcomers”, “no position changes”, “otherwise steady”). Save “held steady / quiet” language for months that truly are quiet across the horizons that matter.
6. **Numbers are optional seasoning.** A round figure can help (“about 27%”, “up roughly 1.5 pp this month”, “up several points over the past year”) but the paragraph should still read if the numbers were removed. Do not write spreadsheet output.
7. **Tone.** Neutral, concise, present-tense or simple past. No hype (“crushes”, “destroys”), no speculation about why (jobs, AI, frameworks) unless it is obvious from the data alone — and even then prefer describing the data. In the **published** text, say “this month”, “over the past year”, “five years ago”—never jargon like “horizons” or “1m/1y/5y”.
8. **Length.** About **20–100 words** (aim for a short paragraph; hard cap ~100, soft floor ~20). Prefer two or three tight sentences over one overloaded sentence when there is more than one beat. No emoji, no markdown, no trailing hashtags.
9. **Consistency.** Same style as existing entries in `headlines.json`. Every month gets a headline. If 1m / 1y / 5y all look uneventful for the leaders, say so — a short “quiet month” blurb is correct; skipping the key is not.

### Good directions (examples of intent)

- A sustained leader widening or narrowing the gap (this month, and in 1y / 5y context when it matters)
- A fast mover in the top 10 whose latest month fits — or breaks — its longer arc
- A multi-year loser or gainer whose 5y decline/rise is the real story (even if 1m/1y look mild)
- A language entering or leaving the top 20 (newcomer, return, or long-time exit) after the move looks durable
- A genuinely quiet month — say that ranking and shares among the leaders held steady

### Avoid

- Auto-picking the max `|Δ pp|` (or max relative %) without asking whether that move is the interesting story
- Headlining a first-time or never-close top-20 appearance before it persists for another month or two
- Negating the news in an active month (“no newcomers”, “ranks unchanged”) when there is already a real story to tell
- Jargon in the public text (“horizons”, “1m/1y/5y”)
- Basing the headline on a non–pull-request metric, or writing as if the chart’s current metric dropdown selected the story
- Correcting or rewriting older months unless fixing a factual error
