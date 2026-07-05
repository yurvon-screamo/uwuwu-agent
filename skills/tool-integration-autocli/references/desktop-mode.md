# Desktop Mode Reference

Interact with desktop applications via autocli. Requires the target app to be running.

## Supported Apps

| App | Status Command |
|-----|---------------|
| Cursor | `autocli cursor status` |
| Codex | `autocli codex status` |
| Notion | `autocli notion status` |
| ChatGPT | `autocli chatgpt status` |
| Discord | `autocli discord-app status` |
| ChatWise | `autocli chatwise status` |
| Doubao App | `autocli doubao-app status` |
| Antigravity | `autocli antigravity status` |

## Common Patterns

### IDE tools (Cursor, Codex)

```bash
# Status check
autocli <app> status

# Conversation
autocli <app> new                      # new conversation
autocli <app> send --text <str>        # send message
autocli <app> read                      # read response
autocli <app> ask --text <str>         # send + read
autocli <app> dump                      # dump full conversation
autocli <app> history --limit N        # conversation history
autocli <app> export                    # export conversation
autocli <app> screenshot                # take screenshot
autocli <app> extract-code             # extract code blocks from response

# Model switching
autocli <app> model --name <str>

# Cursor-specific
autocli cursor composer                 # open composer
```

### Notion

```bash
autocli notion status                   # app status
autocli notion search --query <str>     # search pages
autocli notion read --id <page_id>      # read page content
autocli notion new --title <str>        # create new page
autocli notion write --id <page_id> --content <str>  # write to page
autocli notion sidebar                   # list sidebar
autocli notion favorites                 # list favorites
autocli notion export --id <page_id>     # export page
```

### Discord

```bash
autocli discord-app status
autocli discord-app servers
autocli discord-app channels --server <id>
autocli discord-app members --server <id>
autocli discord-app send --channel <id> --text <str>
autocli discord-app read --channel <id> --limit N
autocli discord-app search --query <str>
```

### AI Assistants (ChatGPT, ChatWise, Doubao, Antigravity)

```bash
autocli <app> status
autocli <app> new
autocli <app> send --text <str>
autocli <app> read
autocli <app> ask --text <str>
autocli <app> dump
autocli <app> screenshot
autocli <app> history --limit N
autocli <app> export
autocli <app> model --name <str>         # ChatWise, Antigravity, Doubao
```

## Requirements

- Target app must be open/running
- No Chrome browser needed for desktop mode
- Works on macOS, Windows, and Linux (where the app runs)
