# GitHub Event Types

The archive CSV contains one row per unique `(actor, repo, event_type, action)` tuple per month.
`produce_statistics` uses three event types:

| Event type | Used for |
|---|---|
| `PullRequestEvent` | `pr-count`, `developer-activity` |
| `IssuesEvent` | `issue-count` |
| `PushEvent` | `push-count` |

Full list of event types that appear in GH Archive data:

| Event type | Actions | Description |
|---|---|---|
| `CommitCommentEvent` | `created` | A comment was posted on a commit. |
| `CreateEvent` | *(none)* | A branch, tag, or repository was created. |
| `DeleteEvent` | *(none)* | A branch or tag was deleted. |
| `DiscussionEvent` | `created` | A discussion was created in a repository. |
| `ForkEvent` | `forked` | A user forked a repository. |
| `GollumEvent` | *(none)* | A wiki page was created or updated. |
| `IssueCommentEvent` | `created` | A comment was posted on an issue or pull request. |
| `IssuesEvent` | `opened`, `closed`, `reopened`, `assigned`, `unassigned`, `labeled`, `unlabeled` | Activity on an issue. |
| `MemberEvent` | `added` | A collaborator was added to a repository. |
| `PublicEvent` | *(none)* | A private repository was made public. |
| `PullRequestEvent` | `opened`, `closed`, `reopened`, `assigned`, `unassigned`, `labeled`, `unlabeled` | Activity on a pull request. |
| `PullRequestReviewEvent` | `created`, `updated`, `dismissed` | A pull request review was submitted, updated, or dismissed. |
| `PullRequestReviewCommentEvent` | `created` | A comment was posted on a pull request diff. |
| `PushEvent` | *(none)* | One or more commits were pushed to a branch or tag. |
| `ReleaseEvent` | `published` | A release was published. |
| `WatchEvent` | `started` | A user starred a repository (the API calls this "watching"). |

> **Note:** Events with no action have an empty `action` column in the CSV.
