# ia — Claude Code Instructions

Read and follow [`AGENTS.md`](./AGENTS.md) for project rules, architecture, and workflow.

- NEVER use `EnterWorktree` to create worktrees — it puts them in `.claude/worktrees/` which `ia-cleanup` can't find. ALWAYS create with `scripts/ia-worktree`, then switch with `EnterWorktree`.
- When you write a design doc or plan, commit it immediately — don't wait for implementation.
- Never clean up the worktree you're currently in. Recommend `scripts/ia-cleanup <slug>` and let the user run it after exiting.
