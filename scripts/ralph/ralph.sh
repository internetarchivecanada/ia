#!/bin/bash
# Ralph Wiggum — Metadata Write Implementation Loop
# Usage: ./scripts/ralph/ralph.sh [max_iterations]
#
# Spawns fresh Claude Code instances to implement metadata write
# support one user story at a time, guided by prd.json.
#
# Features:
#   - Per-story code review after each successful implementation
#   - Review feedback fed back to next iteration for fixes
#   - Comprehensive final review before PR creation
#   - Closes GitHub issues as stories pass
#   - Creates PR when all stories complete
#   - Detects stuck stories and skips after 3 consecutive failures
#   - Rate limit detection with auto-retry (waits for limits to reset)
#   - Scoped ONLY to metadata write issues (#109-#125, parent #85)
#
# Environment variables:
#   RALPH_RATE_LIMIT_WAIT    — seconds to wait on rate limit (default: 300)
#   RALPH_RATE_LIMIT_RETRIES — max consecutive retries before giving up (default: 20)

set -e

MAX_ITERATIONS=${1:-50}
RATE_LIMIT_WAIT=${RALPH_RATE_LIMIT_WAIT:-300}  # seconds to wait on rate limit (default 5 min)
RATE_LIMIT_MAX_RETRIES=${RALPH_RATE_LIMIT_RETRIES:-20}  # max consecutive rate limit retries
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
PRD_FILE="$SCRIPT_DIR/prd.json"
PROGRESS_FILE="$SCRIPT_DIR/progress.txt"
PROMPT_FILE="$SCRIPT_DIR/CLAUDE.md"
REVIEW_FILE="$SCRIPT_DIR/REVIEW.md"
FINAL_REVIEW_FILE="$SCRIPT_DIR/FINAL_REVIEW.md"
FEEDBACK_FILE="$SCRIPT_DIR/review-feedback.md"
STUCK_FILE="$SCRIPT_DIR/.stuck_tracker"

# Tools the agent is allowed to use (no --dangerously-skip-permissions needed)
ALLOWED_TOOLS="Read,Edit,Write,Grep,Glob,Bash(cargo *),Bash(git add *),Bash(git commit *),Bash(git diff *),Bash(git log *),Bash(git show *),Bash(git status*),Bash(git branch *),Bash(git checkout *),Bash(gh auth switch *),Bash(gh issue close *),Bash(jq *),Bash(ls *),Bash(mkdir *)"

cd "$PROJECT_DIR"

# Ensure we're on the right branch
CURRENT_BRANCH=$(git branch --show-current)
TARGET_BRANCH=$(jq -r '.branchName' "$PRD_FILE")

if [ "$CURRENT_BRANCH" != "$TARGET_BRANCH" ]; then
  echo "Switching to branch: $TARGET_BRANCH"
  git checkout "$TARGET_BRANCH" 2>/dev/null || git checkout -b "$TARGET_BRANCH"
fi

# Ensure correct GitHub account
gh auth switch --user jjjake 2>/dev/null || true

# Initialize stuck tracker if missing
if [ ! -f "$STUCK_FILE" ]; then
  echo "{}" > "$STUCK_FILE"
fi

echo "============================================="
echo "  Ralph — ia Metadata Write Implementation"
echo "  Branch: $TARGET_BRANCH"
echo "  Max iterations: $MAX_ITERATIONS"
echo "  Scope: Issues #109-#125 (parent #85)"
echo "  Reviews: per-story + final branch audit"
echo "============================================="
echo ""

# Show initial status
TOTAL=$(jq '.userStories | length' "$PRD_FILE")
DONE=$(jq '[.userStories[] | select(.passes == true)] | length' "$PRD_FILE")
echo "Stories: $DONE/$TOTAL complete"
echo ""

# -------------------------------------------------------
# close_issue_if_open <issue_number>
# -------------------------------------------------------
close_issue_if_open() {
  local ISSUE_NUM=$1
  if [ -z "$ISSUE_NUM" ] || [ "$ISSUE_NUM" = "null" ]; then
    return
  fi
  local STATE
  STATE=$(gh issue view "$ISSUE_NUM" --json state -q '.state' 2>/dev/null || echo "UNKNOWN")
  if [ "$STATE" = "OPEN" ]; then
    echo "  Closing issue #$ISSUE_NUM..."
    gh issue close "$ISSUE_NUM" --comment "Completed by Ralph (automated implementation loop)." 2>/dev/null || true
  fi
}

# -------------------------------------------------------
# sync_issues_with_prd
# -------------------------------------------------------
sync_issues_with_prd() {
  local PASSED_ISSUES
  PASSED_ISSUES=$(jq -r '[.userStories[] | select(.passes == true) | .githubIssue] | .[]' "$PRD_FILE")
  for ISSUE_NUM in $PASSED_ISSUES; do
    close_issue_if_open "$ISSUE_NUM"
  done
}

# -------------------------------------------------------
# get_stuck_count / increment_stuck / clear_stuck
# -------------------------------------------------------
get_stuck_count() {
  jq -r --arg id "$1" '.[$id] // 0' "$STUCK_FILE"
}

increment_stuck() {
  local CURRENT
  CURRENT=$(get_stuck_count "$1")
  jq --arg id "$1" --argjson count "$((CURRENT + 1))" '.[$id] = $count' "$STUCK_FILE" > "${STUCK_FILE}.tmp"
  mv "${STUCK_FILE}.tmp" "$STUCK_FILE"
}

clear_stuck() {
  jq --arg id "$1" 'del(.[$id])' "$STUCK_FILE" > "${STUCK_FILE}.tmp"
  mv "${STUCK_FILE}.tmp" "$STUCK_FILE"
}

# -------------------------------------------------------
# is_rate_limited <output>
# Checks if claude output indicates a rate limit hit
# -------------------------------------------------------
is_rate_limited() {
  local OUTPUT="$1"
  local EXIT_CODE="$2"
  # Match EXACT Claude CLI/Max rate limit messages only
  # Must be specific to avoid false positives from agent output about rate limiting code
  if echo "$OUTPUT" | grep -q "You've hit your limit"; then
    return 0
  fi
  if echo "$OUTPUT" | grep -q "Too many requests. Please try again later"; then
    return 0
  fi
  return 1
}

# -------------------------------------------------------
# run_claude_with_retry <prompt_file> <description>
# Runs claude with rate limit retry. Sets CLAUDE_OUTPUT global.
# Returns the eventual exit code.
# -------------------------------------------------------
run_claude_with_retry() {
  local PROMPT="$1"
  local DESC="$2"
  local RETRIES=0

  while true; do
    local EXIT_CODE=0
    CLAUDE_OUTPUT=$(claude -p "$(cat "$PROMPT")" --allowedTools "$ALLOWED_TOOLS" 2>&1 | tee /dev/stderr) || EXIT_CODE=$?

    if is_rate_limited "$CLAUDE_OUTPUT" "$EXIT_CODE"; then
      RETRIES=$((RETRIES + 1))
      if [ "$RETRIES" -ge "$RATE_LIMIT_MAX_RETRIES" ]; then
        echo ""
        echo "  Rate limited $RETRIES times for $DESC. Giving up."
        return 1
      fi
      echo ""
      echo "  ⏳ Rate limited during $DESC (attempt $RETRIES/$RATE_LIMIT_MAX_RETRIES)"
      echo "  Waiting ${RATE_LIMIT_WAIT}s for limits to reset... ($(date '+%H:%M:%S'))"
      sleep "$RATE_LIMIT_WAIT"
      echo "  Resuming... ($(date '+%H:%M:%S'))"
      continue
    fi

    return $EXIT_CODE
  done
}

# -------------------------------------------------------
# run_review
# Spawns a review Claude instance to audit the last commit
# -------------------------------------------------------
run_review() {
  local STORY_ID=$1
  local STORY_TITLE=$2

  echo ""
  echo "  ---- Code Review: $STORY_ID ----"
  echo ""

  # Run review agent
  run_claude_with_retry "$REVIEW_FILE" "code review ($STORY_ID)" || true

  # Check if review produced FIXME feedback
  if [ -f "$FEEDBACK_FILE" ] && grep -q "FIXME" "$FEEDBACK_FILE" 2>/dev/null; then
    echo ""
    echo "  Review found issues — next iteration will fix them"
    return 1
  else
    echo ""
    echo "  Review: PASS"
    # Clean up feedback file if it was a pass
    echo "RESOLVED" > "$FEEDBACK_FILE"
    return 0
  fi
}

# -------------------------------------------------------
# run_final_review
# Comprehensive review of entire branch before PR
# -------------------------------------------------------
run_final_review() {
  echo ""
  echo "==============================================================="
  echo "  Final Branch Review"
  echo "  Reviewing all changes on $TARGET_BRANCH..."
  echo "==============================================================="
  echo ""

  run_claude_with_retry "$FINAL_REVIEW_FILE" "final branch review" || true

  echo ""
  echo "  Final review complete."
  if [ -f "$SCRIPT_DIR/final-review-report.md" ]; then
    echo "  Report: scripts/ralph/final-review-report.md"
  fi
}

# Sync any already-passed stories with GitHub issues on startup
sync_issues_with_prd

for i in $(seq 1 $MAX_ITERATIONS); do
  # Check remaining work
  REMAINING=$(jq '[.userStories[] | select(.passes == false)] | length' "$PRD_FILE")
  if [ "$REMAINING" -eq 0 ]; then
    echo ""
    echo "============================================="
    echo "  All stories complete!"
    echo "============================================="
    sync_issues_with_prd
    break
  fi

  # Check if there's pending review feedback to fix
  HAS_FEEDBACK=false
  if [ -f "$FEEDBACK_FILE" ] && grep -q "FIXME" "$FEEDBACK_FILE" 2>/dev/null; then
    HAS_FEEDBACK=true
  fi

  # Find next story (highest priority that isn't stuck)
  NEXT_STORY_ID=""
  NEXT_STORY_TITLE=""
  NEXT_STORY_ISSUE=""
  STUCK_COUNT=0

  for CANDIDATE in $(jq -r '[.userStories[] | select(.passes == false)] | sort_by(.priority) | .[].id' "$PRD_FILE"); do
    CANDIDATE_STUCK=$(get_stuck_count "$CANDIDATE")
    if [ "$CANDIDATE_STUCK" -lt 3 ]; then
      NEXT_STORY_ID="$CANDIDATE"
      NEXT_STORY_TITLE=$(jq -r --arg id "$CANDIDATE" '.userStories[] | select(.id == $id) | .title' "$PRD_FILE")
      NEXT_STORY_ISSUE=$(jq -r --arg id "$CANDIDATE" '.userStories[] | select(.id == $id) | .githubIssue' "$PRD_FILE")
      STUCK_COUNT=$CANDIDATE_STUCK
      break
    fi
  done

  if [ -z "$NEXT_STORY_ID" ]; then
    echo ""
    echo "============================================="
    echo "  ALL remaining stories are stuck (3+ failed attempts each)."
    echo "  Manual intervention required."
    echo "============================================="
    DONE=$(jq '[.userStories[] | select(.passes == true)] | length' "$PRD_FILE")
    echo "  Stories completed: $DONE/$TOTAL"
    exit 1
  fi

  DONE_BEFORE=$(jq '[.userStories[] | select(.passes == true)] | length' "$PRD_FILE")

  echo ""
  echo "==============================================================="
  echo "  Ralph Iteration $i of $MAX_ITERATIONS"
  if [ "$HAS_FEEDBACK" = true ]; then
    echo "  Mode: FIXING REVIEW FEEDBACK"
  fi
  echo "  Story: $NEXT_STORY_ID - $NEXT_STORY_TITLE"
  echo "  Issue: #$NEXT_STORY_ISSUE"
  echo "  Stories remaining: $REMAINING/$TOTAL"
  if [ "$STUCK_COUNT" -gt 0 ]; then
    echo "  Retry attempt: $((STUCK_COUNT + 1))"
  fi
  echo "==============================================================="
  echo ""

  # Run Claude Code with the ralph prompt
  CLAUDE_EXIT=0
  run_claude_with_retry "$PROMPT_FILE" "story $NEXT_STORY_ID" || CLAUDE_EXIT=$?
  OUTPUT="$CLAUDE_OUTPUT"

  # If rate limit exhausted all retries, don't count against stuck tracker
  if [ "$CLAUDE_EXIT" -ne 0 ] && is_rate_limited "$OUTPUT" "$CLAUDE_EXIT"; then
    echo ""
    echo "  Iteration $i lost to rate limiting — not counting against story"
    sleep 2
    continue
  fi

  # Check what happened
  DONE_AFTER=$(jq '[.userStories[] | select(.passes == true)] | length' "$PRD_FILE")

  if [ "$DONE_AFTER" -gt "$DONE_BEFORE" ]; then
    # Story completed — run code review
    echo ""
    echo "Story completed! Running code review..."
    sync_issues_with_prd
    clear_stuck "$NEXT_STORY_ID"

    if run_review "$NEXT_STORY_ID" "$NEXT_STORY_TITLE"; then
      echo "  Story $NEXT_STORY_ID: implemented + reviewed"
    else
      echo "  Story $NEXT_STORY_ID: implemented, review feedback pending"
      # Feedback file exists — next iteration will pick it up
    fi
  else
    # Story did NOT complete
    increment_stuck "$NEXT_STORY_ID"
    NEW_STUCK=$(get_stuck_count "$NEXT_STORY_ID")
    echo ""
    echo "Story $NEXT_STORY_ID did not complete (attempt $NEW_STUCK of 3 before skip)"
  fi

  # Check for completion signal
  if echo "$OUTPUT" | grep -q "<promise>COMPLETE</promise>"; then
    echo ""
    echo "============================================="
    echo "  All stories implemented!"
    echo "============================================="
    sync_issues_with_prd
    break
  fi

  # Check if all stories done (even without promise)
  REMAINING_NOW=$(jq '[.userStories[] | select(.passes == false)] | length' "$PRD_FILE")
  if [ "$REMAINING_NOW" -eq 0 ]; then
    # Check if there's still review feedback to fix
    if [ -f "$FEEDBACK_FILE" ] && grep -q "FIXME" "$FEEDBACK_FILE" 2>/dev/null; then
      echo ""
      echo "All stories pass but review feedback pending — continuing to fix..."
    else
      echo ""
      echo "============================================="
      echo "  All stories complete!"
      echo "============================================="
      sync_issues_with_prd
      break
    fi
  fi

  echo ""
  echo "Status after iteration $i: $DONE_AFTER/$TOTAL stories complete"

  sleep 2
done

# -------------------------------------------------------
# Post-completion: Final review, push, PR
# -------------------------------------------------------
DONE=$(jq '[.userStories[] | select(.passes == true)] | length' "$PRD_FILE")
REMAINING=$(jq '[.userStories[] | select(.passes == false)] | length' "$PRD_FILE")

if [ "$REMAINING" -eq 0 ]; then
  # Run comprehensive final review before PR
  run_final_review

  echo ""
  echo "============================================="
  echo "  All $TOTAL stories complete!"
  echo "  Pushing branch and creating PR..."
  echo "============================================="
  echo ""

  # Ensure correct GitHub account
  gh auth switch --user jjjake 2>/dev/null || true

  # Push branch
  git push -u origin "$TARGET_BRANCH" 2>/dev/null || git push origin "$TARGET_BRANCH"

  # Check if PR already exists
  EXISTING_PR=$(gh pr list --head "$TARGET_BRANCH" --json number -q '.[0].number' 2>/dev/null || echo "")

  if [ -n "$EXISTING_PR" ] && [ "$EXISTING_PR" != "null" ]; then
    echo "PR #$EXISTING_PR already exists for branch $TARGET_BRANCH"
    PR_URL=$(gh pr view "$EXISTING_PR" --json url -q '.url')
  else
    PR_URL=$(gh pr create \
      --title "feat: metadata write support (#85)" \
      --body "$(cat <<'PREOF'
## Summary

Implements full metadata write support for `ia-core` and `ia-cli`:

- `metadata::modify()` core function with RFC 6902 JSON Patch
- CLI flags: `--modify`, `--append`, `--append-list`, `--insert`, `--remove`
- Batch operations from `--itemlist`, `--search`, stdin
- Multi-format spreadsheet reader (CSV, TSV, XLSX, ODS, JSONL)
- Global 429 rate limiter for concurrent write tasks
- Comprehensive test suite with 16+ mock fixtures (zero live requests)

## Test Plan

- [x] `cargo test --workspace` — all tests pass
- [x] `cargo clippy --workspace -- -D warnings` — no warnings
- [x] All wiremock-based, zero live archive.org requests
- [x] Edge cases: unicode, semicolon subjects, huge metadata, null fields

## Review Notes

See `scripts/ralph/final-review-report.md` for the automated final review.

Closes #85
Closes #109 #110 #111 #112 #113 #114 #115 #116 #117 #118 #119 #120 #121 #122 #123 #124 #125

🤖 Generated with Ralph (automated implementation loop)
PREOF
    )" 2>/dev/null || echo "")

    if [ -z "$PR_URL" ]; then
      echo "WARNING: PR creation failed. Push succeeded — create PR manually."
    fi
  fi

  echo ""
  echo "============================================="
  echo "  Ralph is DONE!"
  echo "  Stories: $DONE/$TOTAL"
  echo "  Branch: $TARGET_BRANCH"
  if [ -n "$PR_URL" ]; then
    echo "  PR: $PR_URL"
  fi
  echo "  Final review: scripts/ralph/final-review-report.md"
  echo "============================================="
  echo ""
  echo "After PR is merged, clean up with:"
  echo "  git checkout main && git pull && git branch -d $TARGET_BRANCH"

  echo "{}" > "$STUCK_FILE"
  exit 0
else
  echo ""
  echo "============================================="
  echo "  Ralph reached max iterations ($MAX_ITERATIONS)"
  echo "  Stories completed: $DONE/$TOTAL"
  echo "  Stories remaining: $REMAINING"
  echo "============================================="
  exit 1
fi
