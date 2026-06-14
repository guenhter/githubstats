# GitHub GraphQL Query Examples

All queries can be executed with `curl`. Set your token first:

```bash
export GITHUB_TOKEN=your_personal_access_token
```

Then use this pattern (replace the query string as needed):

```bash
curl -s -X POST https://api.github.com/graphql \
  -H "Authorization: Bearer $GITHUB_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"query": "{ YOUR_QUERY_HERE }"}' \
  | jq .
```

---

## Latest 10 merged pull requests (global)

Returns the 10 most recently merged PRs across all of GitHub, including changed files.

```graphql
{
  search(query: "is:pr is:merged sort:updated-desc -author:app/dependabot -author:app/renovate -author:app/github-actions", type: ISSUE, first: 10) {
    nodes {
      ... on PullRequest {
        number
        title
        mergedAt
        author { login }
        repository { nameWithOwner }
        additions
        deletions
        changedFiles
        files(first: 5) {
          nodes {
            path
            changeType
            additions
            deletions
          }
        }
      }
    }
  }
}
```

## Count merged pull requests in a date range

Returns the total number of merged PRs in April 2026. No nodes are fetched (`first: 0`), only the count.

```graphql
{
  search(query: "is:pr is:merged merged:2026-04-01..2026-04-30 -author:app/dependabot -author:app/renovate -author:app/github-actions", type: ISSUE, first: 0) {
    issueCount
  }
}
```

> `issueCount` reflects the total matches for the search query. GitHub caps this value at 10,000,000.

The `merged:` filter accepts any date range in `YYYY-MM-DD..YYYY-MM-DD` format. You can further narrow results with additional filters, e.g.:
- `language:rust` — only repos with Rust as primary language
- `repo:owner/name` — scoped to a specific repository
- `author:login` — PRs by a specific user

## Count merged pull requests for a specific language in a date range

Returns the total number of merged PRs in Rust repos in April 2026.

```graphql
{
  search(query: "is:pr is:merged merged:2026-04-01..2026-04-30 language:rust -author:app/dependabot -author:app/renovate -author:app/github-actions", type: ISSUE, first: 0) {
    issueCount
  }
}
```

## Count merged pull requests for multiple languages at once (batched)

Using **GraphQL aliases**, multiple language counts can be fetched in a single API call.

```graphql
{
  rust:       search(query: "is:pr is:merged merged:2026-04-01..2026-04-30 language:rust       -author:app/dependabot -author:app/renovate -author:app/github-actions", type: ISSUE, first: 0) { issueCount }
  python:     search(query: "is:pr is:merged merged:2026-04-01..2026-04-30 language:python     -author:app/dependabot -author:app/renovate -author:app/github-actions", type: ISSUE, first: 0) { issueCount }
  javascript: search(query: "is:pr is:merged merged:2026-04-01..2026-04-30 language:javascript -author:app/dependabot -author:app/renovate -author:app/github-actions", type: ISSUE, first: 0) { issueCount }
  typescript: search(query: "is:pr is:merged merged:2026-04-01..2026-04-30 language:typescript -author:app/dependabot -author:app/renovate -author:app/github-actions", type: ISSUE, first: 0) { issueCount }
  go:         search(query: "is:pr is:merged merged:2026-04-01..2026-04-30 language:go         -author:app/dependabot -author:app/renovate -author:app/github-actions", type: ISSUE, first: 0) { issueCount }
}
```

Example result for April 2026:

| Language   | Merged PRs |
|------------|------------|
| TypeScript | 2,463,701  |
| Python     | 1,623,786  |
| JavaScript |   958,784  |
| Go         |   549,240  |
| Rust       |   431,705  |


