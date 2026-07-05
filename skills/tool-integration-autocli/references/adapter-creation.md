# Adapter Creation Guide

Creating YAML adapters for unsupported websites, DOM exploration patterns.

## Auto-Generation

```bash
autocli generate <url>           # attempt automatic generation
autocli generate <url> --goal "extract top articles"
```

If successful, the adapter is created at `~/.autocli/adapters/<site>/` and ready to use immediately.

## Manual YAML Adapter

### Location

```
~/.autocli/adapters/<site>/<command>.yaml
```

### Template

```yaml
site: sitename
name: command-name
description: What this command does
domain: example.com
strategy: public          # or "authenticated"
browser: true              # true = needs Chrome, false = public API

args:
  limit:
    type: int
    default: 10
  query:
    type: string
    required: false

pipeline:
  - navigate: https://example.com/path
  - wait: 1000                          # optional: wait for JS render (ms)
  - evaluate: |
      (async () => {
        const limit = ${{ args.limit }};
        const results = [];
        document.querySelectorAll('.item-class').forEach((el, i) => {
          if (i >= limit) return;
          results.push({
            title: el.querySelector('.title')?.textContent?.trim(),
            url: el.querySelector('a')?.href,
            rank: i + 1
          });
        });
        return results;
      })()

columns: [rank, title, url]
```

### DOM Exploration Workflow

1. **Navigate** to the target page in Chrome
2. **Inspect** the DOM structure (DevTools)
3. **Find stable selectors** in priority order:
   - `data-test` attributes (most stable)
   - Semantic class names (e.g. `.post-title`, `.article-item`)
   - Structural selectors (e.g. `article > h2 > a`)
4. **Build the evaluate script** using those selectors
5. **Test** with `autocli <site> <command> --format json`
6. **Iterate** if output is wrong or incomplete

### Selector Stability Tips

| Stability | Pattern | Example |
|-----------|---------|---------|
| High | `data-test` attributes | `el.querySelector('[data-test="post-title"]')` |
| Medium | BEM/semantic classes | `el.querySelector('.post-item__title')` |
| Low | Generic classes | `el.querySelector('.card .text-lg')` |
| Avoid | Index-based | `el.children[2].querySelector('span')` |

### Common Patterns

```yaml
# Simple list extraction
pipeline:
  - navigate: https://example.com/hot
  - evaluate: |
      (async () => {
        return [...document.querySelectorAll('.item')].slice(0, ${{ args.limit }}).map(el => ({
          title: el.querySelector('h2')?.textContent?.trim(),
          url: el.querySelector('a')?.href
        }));
      })()

# With click-through to load more
pipeline:
  - navigate: https://example.com
  - evaluate: |
      (async () => {
        const button = document.querySelector('.load-more');
        if (button) button.click();
        await new Promise(r => setTimeout(r, 2000));
        return [...document.querySelectorAll('.item')].map(el => ({...}));
      })()

# From API response in page
pipeline:
  - navigate: https://example.com/api/data
  - evaluate: |
      (async () => {
        return JSON.parse(document.body.textContent);
      })()
```

### Deduplication

When selectors match duplicate elements:

```javascript
const seen = new Set();
return [...document.querySelectorAll('.item')]
  .filter(el => {
    const url = el.querySelector('a')?.href;
    if (seen.has(url)) return false;
    seen.add(url);
    return true;
  })
  .slice(0, limit)
  .map(el => ({...}));
```

### Strategy Choice

| `strategy` | When to use |
|------------|-------------|
| `public` | Public content, no login needed (or browser handles login) |
| `authenticated` | Must be logged in, adapter checks session |
