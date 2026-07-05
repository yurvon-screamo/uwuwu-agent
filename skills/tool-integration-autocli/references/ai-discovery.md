# AI Discovery Commands

Deep dive into `autocli explore`, `autocli cascade`, and `autocli generate`.

## `autocli explore <url>`

Explores a website's API structure. Useful for understanding what endpoints and data patterns a site exposes before building an adapter.

```bash
autocli explore https://example.com
```

Returns information about:
- API endpoints detected
- Data patterns (pagination, auth requirements)
- Request/response structure hints

## `autocli cascade <url>`

Auto-detects authentication strategies used by a website. Helps determine what `strategy` to set in a YAML adapter.

```bash
autocli cascade https://example.com
```

Detects:
- Cookie-based auth
- Token-based auth (Bearer, API key)
- OAuth flows
- Session-based auth
- None (public)

## `autocli generate <url>`

Automatically generates a YAML adapter for a website. This is the first thing to try when a site is not yet supported.

```bash
autocli generate https://example.com
autocli generate https://example.com --goal "extract top articles"
```

The `--goal` hint helps the generator focus on the right data patterns.

### When to use `--goal`

| Goal pattern | Example |
|---|---|
| General listing | `--goal "list all items"` |
| Article extraction | `--goal "extract articles"` |
| Search results | `--goal "search results for keyword"` |
| User profiles | `--goal "user profile page"` |
| API documentation | `--goal "API docs"` |

### After generation

1. Check the generated adapter at `~/.autocli/adapters/<site>/`
2. Verify: `autocli <site> <command> --format json`
3. If output is wrong, edit the YAML manually (see `adapter-creation.md`)
4. If generation failed entirely, create the adapter from scratch

## Decision Flow

```text
New site needed?
├─ autocli <site> --help     → commands exist? use them
├─ autocli generate <url>     → success? verify and use
│   └─ failed? → autocli explore <url> + autocli cascade <url>
│       └─ manually create YAML adapter (see adapter-creation.md)
└─ autocli doctor              → if anything seems broken
```
