---
name: flow-merge-review
description: Invoke this skill when you need to conduct a deep review of a Merge Request or Pull Request.
---

# flow-merge-review

Your task is to conduct a comprehensive audit of changes in an MR/PR. You must not simply check syntax — understand the "why" behind the changes, whether they align with business goals, and whether they introduce over-engineering.

**Speeding up processes that are not the bottleneck only increases waste.** Every change accepted into the codebase must be justified — incorrect code will become a template for future changes.

## Workflow

### Step 1: Context Gathering

Before looking at the code, understand the goals of the changes:
- **MR/PR description analysis**: What exactly was changed, why, and what is the expected outcome.
- **External sources (Jira/Linear)**: If the description contains a link to a task, go there. Understand the original problem or feature.
- **Diff Analysis**: Review the overall diff of changes to understand the scope and affected modules.

### Step 2: Deep Dive

- **Branch switch**: Switch to the MR/PR branch locally.
- **File system inspection**: Study the changed files in the context of the project. See how new components fit into the existing hierarchy.
- **Run tools**: Use `qlty` to get metrics and find code smells in the changed files.

### Step 3: Final Synthesis

Analyze all changes independently and produce a final report.

## Output Format

### Final MR/PR Review Report

```markdown
# 🔍 Merge Request Review: [MR Title]

## 🎯 Verdict
**[APPROVED / REQUEST CHANGES / COMMENT]**

### 📝 Change Overview
[Brief description of what was done and why, based on task context]

### 🚩 Key Observations (Critical Issues)
1. **[Title]**: [Description of the issue, why it matters]
2. ...

### 💡 Improvement Suggestions / Over-engineering
- [Did we spot code that isn't needed to solve the task?]
- [Could it be done more simply?]

### ✅ Positive Aspects
- [What was done particularly well]
```

## Review Principles

1. **Justification**: Always ask "Is this change needed at all?". We are fighting codebase bloat.
2. **Context above all**: A review without understanding the business task is just a lint check.
3. **Constructiveness**: Every observation should come with an explanation of "why" and a suggestion of "how to improve".
4. **Objectivity**: Use `qlty` metrics to back up observations about code complexity.
