## 1. PRINCIPLES OF HUMAN WRITING

### ✅ Do this

| Principle         | Description                                      | Example                                      |
|-------------------|--------------------------------------------------|----------------------------------------------|
| **Specificity**   | Precise facts, figures, dates, names             | "Founded on March 15, 1989 in Barcelona"     |
| **Neutrality**    | No value judgments or advertising                | "The company produces 500 units per month"   |
| **Naturalness**   | Write like a human for a human                   | Vary sentence length                         |
| **Context**       | Explain significance of facts, not just list them| "This reduced processing time by 30%"        |
| **Proportionality**| Don't exaggerate ordinary things                | Don't call a routine event a "turning point" |

### ❌ Don't do this

| Mistake                | Why it's bad                          | How to fix                  |
|------------------------|---------------------------------------|-----------------------------|
| Regression to the mean | Smoothing facts into generic phrases  | Bring back specific details |
| Inflating significance | Every event is "historic"             | Assess real importance      |
| Formulaic conclusions  | "Despite successes, there are problems..." | Avoid formulaic endings |
| Vague attribution      | "Experts say" without names           | Name specific sources       |

## 2. CONTROLLED LANGUAGE STANDARDS

Use these when writing technical documentation, instructions, tool descriptions, error messages or agent-facing text.

### 2.1 English → ASD-STE100 (Simplified Technical English)

Apply when the text is in English and must be unambiguous (especially for non-native readers or other agents).

**Core rules (Issue 9 style):**
- Procedures: max **20 words** per sentence, one instruction per sentence.
- Descriptions: max **25 words** per sentence.
- Active voice. Imperative for instructions.
- One meaning per word. Prefer the simplest approved word.
- No stacked auxiliaries (should/would/may/might), no -ing clauses that hide the subject.
- No long noun clusters.
- Consistent terminology: one name for one thing.

**Trigger phrases for the model:**
- "Rewrite in ASD-STE100"
- "Apply STE100"
- "Make this STE-compliant"
- "80% of the way to ASD-STE100" (softer version)

**Recommended open skills:**
- `npx skills add danyuchn/asd-ste100-skill`
- `npx skills add nuelcyoung/asd-ste100`
- `npx skills add blagoySimandov/asd-ste100-writer-skill`

### 2.2 Russian → УТР (Упрощённый технический русский)

Apply when the text is in Russian and must be clear and unambiguous (especially technical documentation).

**Core rules (based on ГОСТ Р 58049-2017 §8.2 + УТР-1 practice):**
- Max **20 words** per sentence.
- One thought / one command per sentence.
- Active voice (действительный залог).
- Instructions: imperative mood, **plural** form, verb first.
- Direct word order (подлежащее не в конце, прямое дополнение не перед сказуемым).
- No participles/деепричастия if they complicate the sentence.
- No passive voice in instructions.
- One term — one meaning. No synonyms for the same concept.
- Max three nouns in a row.
- Short paragraphs (one topic).

**Trigger phrases for the model:**
- "Пиши по правилам УТР"
- "Перепиши в упрощённом техническом русском"
- "Примени УТР (ГОСТ Р 58049-2017)"
- "Приблизься к УТР на 80–90 %"

**Example prompt block:**
```
Пиши строго по правилам Упрощённого технического русского (УТР):
1. Предложения максимум 20 слов.
2. Одна мысль / одна команда в предложении.
3. Действительный залог.
4. В инструкциях — повелительное наклонение множественного числа, глагол на первом месте.
5. Прямой порядок слов.
6. Без причастных и деепричастных оборотов, без страдательного залога.
7. Один термин — одно значение.
8. Не больше трёх существительных подряд.
```

## 3. FORBIDDEN AI-STYLE PATTERNS

### Excessive significance
Avoid: "is a testament/reminder of…", "plays a vital/key/crucial role", "underscores its importance", "marks a turning point", "leaves an indelible mark".

### Advertising language
Avoid: "boasts a", "vibrant", "rich heritage", "nestled in the heart of", "groundbreaking", "renowned", "diverse array".

### Superficial analysis
Avoid ending sentences with vague -ing clauses that add no information.

### Vague attribution
Avoid: "Experts say…", "Observers note…", "Industry reports show…". Name the actual source.

### Formulaic "challenges + future" endings
Avoid the classic AI sandwich: "Despite X, faces challenges… Despite these challenges, continues to… Future investments could…"

## 4. AI-CLICHÉ DICTIONARY

### High risk (avoid always)
delve, tapestry, testament, pivotal, underscore, showcase, vibrant, landscape (as abstraction), intricate, interplay, fostering, garner, meticulous, enduring, crucial

### Medium risk (use only with concrete metrics)
enhance, highlight, emphasizing, align with, contributing to, symbolizing, reflecting

## 5. STRUCTURAL RECOMMENDATIONS

- Get straight to the facts. No "In this article we explore…".
- Prefer flexible structure over rigid "History → Significance → Challenges → Future".
- Sources: specific name + date + title. Put trivial sources in footnotes.
- Prefer short sentences and concrete numbers over abstract adjectives.

## 6. FINAL PRINCIPLES

1. Specificity over grandiosity — numbers beat adjectives.
2. Names over abstractions — "Prof. Petrov" beats "experts".
3. Facts over interpretations — what happened, not what it means.
4. Footnotes over body text for sources.
5. Controlled language when clarity is critical:
   - English → ASD-STE100
   - Russian → УТР

## 7. QUICK CHECK

Before publishing:
- Run through desloper cli.
- For English technical text — apply ASD-STE100 rules.
- For Russian technical text — apply УТР rules.
- Check that no high-risk AI clichés remain.
