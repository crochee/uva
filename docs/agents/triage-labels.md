# Triage Labels

The skills speak in terms of five canonical triage roles. This file maps those
roles to the strings used in this repo's tracker.

The tracker is local markdown, so these strings are written into the ticket
file instead of being applied as labels: a `Status:` line near the top of
`.scratch/<feature>/issues/<NN>-<slug>.md`, and a `Category:` line for the
category role.

| Role in mattpocock/skills | String in our tracker | Field  | Meaning                                  |
| -------------------------- | -------------------- | ------ | ---------------------------------------- |
| `needs-triage`             | `needs-triage`       | Status | Maintainer needs to evaluate this issue  |
| `needs-info`               | `needs-info`         | Status | Waiting on reporter for more information |
| `ready-for-agent`          | `ready-for-agent`    | Status | Fully specified, ready for an AFK agent  |
| `ready-for-human`          | `ready-for-human`    | Status | Requires human implementation            |
| `wontfix`                  | `wontfix`            | Status | Will not be actioned                     |
| `bug`                      | `bug`                | Category | Something is broken                    |
| `enhancement`              | `enhancement`        | Category | New feature or improvement            |

When a skill mentions a role (e.g. "apply the AFK-ready triage label"), use the
corresponding string from this table.

Edit the "String in our tracker" column to match whatever vocabulary you
actually use.
