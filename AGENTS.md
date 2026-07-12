# Codex Project Guidance

## Skill routing

When the user's request matches an available skill, invoke that skill instead of recreating its workflow manually.

Key routing rules:

- Product ideas and problem framing: use `office-hours`.
- Strategy and scope review: use `plan-ceo-review`.
- Architecture and implementation planning: use `plan-eng-review`.
- Design planning and visual review: use `design-consultation`, `plan-design-review`, or `design-review` as appropriate.
- Full multi-discipline plan review: use `autoplan`.
- Bugs, errors, and regressions: use `investigate` or `diagnosing-bugs`.
- Browser QA and behavior verification: use `qa` or `qa-only`.
- Code or diff review: use `review` or `code-review`.
- Shipping, pull requests, and deployment: use `ship` or `land-and-deploy`.
- Save or restore long-running project context: use `context-save` or `context-restore`.

Follow the selected skill from its first required step, including its decision gates and verification requirements.
