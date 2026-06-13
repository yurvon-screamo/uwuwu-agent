---
name: rules-clean-code
description: Apply these rules to ensure code cleanliness, readability, and maintainability. Use when working with code.
---

# Clean Code Standards

1. **Decomposition (SRP)**: One function — one responsibility.
2. **Size limits**:
   - Recommended function size: ≤ 50 lines.
   - Maximum function size: ≤ 100 lines.
   - Maximum file size: ≤ 300 lines.
3. **Style**:
   - Semantically precise variable and function names (Clean Code).
   - No comments describing what the code does.
   - Comments only for: magic numbers/constants, complex business logic conditions ("Why"), RegExp.
   - Comment language: English.
4. **Components**: 1 component = 1 file.
5. **DRY — knowledge, not code**: DRY applies to domain knowledge, not identical-looking code. Two identical fragments may model different concepts that evolve independently (e.g. shipping address vs. warehouse address). Merging them couples unrelated change rates.
   - Do not extract on the first repetition. Wait for the third occurrence (Rule of Three / AHA — Avoid Hasty Abstractions) and confirm all copies must change together when the underlying rule changes.
   - A correct abstraction has a real domain name (`Money`, `TaxRate`, `InvoiceNumber`). If the best name is `Helper`, `Utils`, or `ProcessData`, you are abstracting form, not knowledge.
   - Do not cross module boundaries with shared code: two modules keeping their own `Order` model is often correct, even if the shapes look alike.
