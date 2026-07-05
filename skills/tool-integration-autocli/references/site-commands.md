# Site Commands Reference

Full command list per site. When the task involves a specific site, load this file for exact command names, required args, and default values.

## Discovery

```bash
autocli list              # all 333 commands
autocli <site> --help     # commands for a specific site
```

## Public Mode (no browser needed)

### HackerNews

| Command | Args | Default |
|---------|------|---------|
| `hackernews top` | `--limit N` | 20 |
| `hackernews new` | `--limit N` | 20 |
| `hackernews best` | `--limit N` | 20 |
| `hackernews ask` | `--limit N` | 20 |
| `hackernews show` | `--limit N` | 20 |
| `hackernews jobs` | `--limit N` | 20 |
| `hackernews search` | `--query <str>`, `--limit N` | 20 |
| `hackernews user` | `--id <username>` | — |

### Dev.to

| Command | Args | Default |
|---------|------|---------|
| `devto top` | `--limit N` | 20 |
| `devto tag` | `--tag <str>`, `--limit N` | 20 |
| `devto user` | `--username <str>` | — |

### Lobsters

| Command | Args | Default |
|---------|------|---------|
| `lobsters hot` | `--limit N` | 20 |
| `lobsters newest` | `--limit N` | 20 |
| `lobsters active` | `--limit N` | 20 |
| `lobsters tag` | `--tag <str>`, `--limit N` | 20 |

### StackOverflow

| Command | Args | Default |
|---------|------|---------|
| `stackoverflow hot` | `--limit N` | 20 |
| `stackoverflow search` | `--query <str>`, `--limit N` | 20 |
| `stackoverflow bounties` | `--limit N` | 20 |
| `stackoverflow unanswered` | `--limit N` | 20 |

### Wikipedia

| Command | Args | Default |
|---------|------|---------|
| `wikipedia search` | `--query <str>`, `--limit N` | 20 |
| `wikipedia summary` | `--title <str>` | — |
| `wikipedia random` | `--limit N` | 20 |
| `wikipedia trending` | `--limit N` | 20 |

### Arxiv

| Command | Args |
|---------|------|
| `arxiv search` | `--query <str>`, `--limit N` |
| `arxiv paper` | `--id <arxiv_id>` |

### BBC / Steam / Hugging Face

| Command | Args |
|---------|------|
| `bbc news` | `--limit N` (max 50) |
| `steam top-sellers` | `--limit N` |
| `hf top` | `--limit N` |

### Apple Podcasts / Xiaoyuzhou / SinaFinance

| Command | Args |
|---------|------|
| `apple-podcasts search` | `--query <str>`, `--limit N` |
| `apple-podcasts episodes` | `--id <podcast_id>`, `--limit N` |
| `apple-podcasts top` | `--limit N` |
| `xiaoyuzhou podcast` | `--id <podcast_id>` |
| `xiaoyuzhou podcast-episodes` | `--id <podcast_id>`, `--limit N` |
| `xiaoyuzhou episode` | `--id <episode_id>` |
| `sinafinance news` | `--limit N` |

### Linux.do

| Command | Args |
|---------|------|
| `linux-do hot` | `--limit N` |
| `linux-do latest` | `--limit N` |
| `linux-do search` | `--query <str>`, `--limit N` |
| `linux-do categories` | — |
| `linux-do category` | `--id <id>`, `--limit N` |
| `linux-do topic` | `--id <id>` |

---

## Public / Browser Mode

### Google

| Command | Args |
|---------|------|
| `google search` | `--query <str>`, `--limit N` |
| `google news` | `--query <str>`, `--limit N` |
| `google suggest` | `--query <str>` |
| `google trends` | `--limit N` |

### V2EX

| Command | Args | Default |
|---------|------|---------|
| `v2ex hot` | `--limit N` | 20 |
| `v2ex latest` | `--limit N` | 20 |
| `v2ex topic` | `--id <topic_id>` | — |
| `v2ex node` | `--name <node>`, `--limit N` | — |
| `v2ex user` | `--username <str>` | — |
| `v2ex member` | `--username <str>` | — |
| `v2ex replies` | `--id <topic_id>` | — |
| `v2ex nodes` | — | — |
| `v2ex daily` | — | — |
| `v2ex me` | — | — |
| `v2ex notifications` | `--limit N` | — |

### Bloomberg

| Command | Args |
|---------|------|
| `bloomberg main` | `--limit N` |
| `bloomberg markets` | `--limit N` |
| `bloomberg economics` | `--limit N` |
| `bloomberg industries` | `--limit N` |
| `bloomberg tech` | `--limit N` |
| `bloomberg politics` | `--limit N` |
| `bloomberg businessweek` | `--limit N` |
| `bloomberg opinions` | `--limit N` |
| `bloomberg feeds` | `--limit N` |
| `bloomberg news` | `--query <str>`, `--limit N` |

---

## Browser Mode (Chrome + extension required)

### Twitter / X

| Command | Args | Write? |
|---------|------|--------|
| `twitter timeline` | `--limit N` (default 20) | no |
| `twitter trending` | `--limit N` | no |
| `twitter search` | `--query <str>`, `--limit N` (default 15) | no |
| `twitter bookmarks` | `--limit N` | no |
| `twitter notifications` | `--limit N` | no |
| `twitter profile` | `--username <handle>`, `--limit N` | no |
| `twitter followers` | `--user <handle>`, `--limit N` | no |
| `twitter following` | `--user <handle>`, `--limit N` | no |
| `twitter thread` | `--url <tweet_url>` | no |
| `twitter article` | `--url <article_url>` | no |
| `twitter post` | `--text <str>` | **yes** |
| `twitter reply` | `--url <tweet_url>`, `--text <str>` | **yes** |
| `twitter like` | `--url <tweet_url>` | **yes** |
| `twitter delete` | `--url <tweet_url>` | **yes** |
| `twitter follow` | `--username <handle>` | **yes** |
| `twitter unfollow` | `--username <handle>` | **yes** |
| `twitter bookmark` | `--url <tweet_url>` | **yes** |
| `twitter unbookmark` | `--url <tweet_url>` | **yes** |
| `twitter download` | `--url <tweet_url>` | no |
| `twitter block` | `--username <handle>` | **yes** |
| `twitter unblock` | `--username <handle>` | **yes** |
| `twitter hide-reply` | `--url <tweet_url>` | **yes** |
| `twitter accept` | — | **yes** |
| `twitter reply-dm` | `--text <str>` | **yes** |

### Bilibili

| Command | Args |
|---------|------|
| `bilibili hot` | `--limit N` (default 20) |
| `bilibili search` | `--keyword <str>`, `--type video|user`, `--page N`, `--limit N` |
| `bilibili me` | — |
| `bilibili favorite` | `--limit N`, `--page N` |
| `bilibili history` | `--limit N` (default 20) |
| `bilibili feed` | `--limit N`, `--type all|video|article` |
| `bilibili subtitle` | `--bvid <bvid>`, `--lang <code>` |
| `bilibili dynamic` | `--limit N` (default 15) |
| `bilibili ranking` | `--limit N` (default 20) |
| `bilibili following` | `--uid <id>`, `--page N`, `--limit N` |
| `bilibili user-videos` | `--uid <id>`, `--limit N`, `--order pubdate|click|stow` |
| `bilibili download` | `--bvid <bvid>` |

### Reddit

| Command | Args | Write? |
|---------|------|--------|
| `reddit hot` | `--subreddit <name>`, `--limit N` | no |
| `reddit frontpage` | `--limit N` (default 15) | no |
| `reddit popular` | `--limit N` | no |
| `reddit search` | `--query <str>`, `--limit N` | no |
| `reddit subreddit` | `--name <sub>`, `--sort hot|new|top|rising`, `--limit N` | no |
| `reddit read` | `--url <post_url>` | no |
| `reddit user` | `--username <str>` | no |
| `reddit user-posts` | `--username <str>`, `--limit N` | no |
| `reddit user-comments` | `--username <str>`, `--limit N` | no |
| `reddit upvote` | `--url <post_url>` | **yes** |
| `reddit save` | `--url <post_url>` | **yes** |
| `reddit comment` | `--url <post_url>`, `--text <str>` | **yes** |
| `reddit subscribe` | `--subreddit <name>` | **yes** |
| `reddit saved` | `--limit N` | no |
| `reddit upvoted` | `--limit N` | no |

### Zhihu

| Command | Args |
|---------|------|
| `zhihu hot` | `--limit N` (default 20) |
| `zhihu search` | `--keyword <str>`, `--limit N` (default 10) |
| `zhihu question` | `--id <question_id>`, `--limit N` |
| `zhihu download` | `--url <zhihu_url>` |

### Xiaohongshu

| Command | Args | Write? |
|---------|------|--------|
| `xiaohongshu search` | `--keyword <str>`, `--limit N` (default 20) | no |
| `xiaohongshu feed` | `--limit N` (default 20) | no |
| `xiaohongshu user` | `--id <user_id>`, `--limit N` | no |
| `xiaohongshu notifications` | `--type mentions|likes|connections`, `--limit N` | no |
| `xiaohongshu download` | `--url <note_url>` | no |
| `xiaohongshu creator-notes` | `--limit N` | no |
| `xiaohongshu creator-note-detail` | `--id <note_id>` | no |
| `xiaohongshu creator-notes-summary` | — | no |
| `xiaohongshu creator-profile` | — | no |
| `xiaohongshu creator-stats` | — | no |
| `xiaohongshu publish` | `--title <str>`, `--content <str>` | **yes** |

### Xueqiu

| Command | Args |
|---------|------|
| `xueqiu feed` | `--page N`, `--limit N` (default 20) |
| `xueqiu hot-stock` | `--limit N` (max 50), `--type 10|12` |
| `xueqiu hot` | `--limit N` |
| `xueqiu search` | `--query <str>`, `--limit N` (default 10) |
| `xueqiu stock` | `--symbol <code>` (e.g. SH600519, AAPL) |
| `xueqiu watchlist` | `--category 1|2|3`, `--limit N` |
| `xueqiu earnings-date` | `--symbol <code>` |

### Weibo / Douban / WeRead

| Command | Args |
|---------|------|
| `weibo hot` | `--limit N` (default 30, max 50) |
| `weibo search` | `--keyword <str>`, `--limit N` |
| `douban search` | `--keyword <str>`, `--limit N` |
| `douban top250` | `--limit N` |
| `douban subject` | `--id <subject_id>` |
| `douban marks` | `--type movie|book`, `--limit N` |
| `douban reviews` | `--id <subject_id>`, `--limit N` |
| `douban movie-hot` | `--limit N` |
| `douban book-hot` | `--limit N` |
| `weread shelf` | — |
| `weread search` | `--keyword <str>`, `--limit N` |
| `weread book` | `--id <book_id>` |
| `weread highlights` | `--id <book_id>` |
| `weread notes` | `--id <book_id>` |
| `weread notebooks` | `--limit N` |
| `weread ranking` | `--limit N` |

### YouTube

| Command | Args |
|---------|------|
| `youtube search` | `--query <str>`, `--limit N` (default 20, max 50) |
| `youtube video` | `--id <video_id>` |
| `youtube transcript` | `--id <video_id>`, `--lang <code>` |

### BOSS直聘

| Command | Args | Write? |
|---------|------|--------|
| `boss search` | `--query <str>`, `--city <city>`, `--experience <exp>`, `--degree <deg>`, `--salary <sal>`, `--limit N` | no |
| `boss detail` | `--id <job_id>` | no |
| `boss recommend` | `--limit N` | no |
| `boss joblist` | `--limit N` | no |
| `boss greet` | `--id <job_id>` | **yes** |
| `boss batchgreet` | `--ids <id1,id2,...>` | **yes** |
| `boss send` | `--id <chat_id>`, `--text <str>` | **yes** |
| `boss chatlist` | `--limit N` | no |
| `boss chatmsg` | `--id <chat_id>`, `--limit N` | no |
| `boss invite` | `--id <job_id>` | **yes** |
| `boss mark` | `--id <chat_id>`, `--label <str>` | **yes** |
| `boss exchange` | `--id <chat_id>` | **yes** |
| `boss resume` | — | no |
| `boss stats` | — | no |

### Facebook / Instagram / TikTok / LinkedIn

| Command | Args | Write? |
|---------|------|--------|
| `facebook feed` | `--limit N` | no |
| `facebook profile` | `--username <str>` | no |
| `facebook search` | `--query <str>`, `--limit N` | no |
| `facebook friends` | `--limit N` | no |
| `facebook groups` | `--limit N` | no |
| `facebook events` | `--limit N` | no |
| `facebook notifications` | `--limit N` | no |
| `facebook memories` | — | no |
| `facebook add-friend` | `--username <str>` | **yes** |
| `facebook join-group` | `--id <group_id>` | **yes** |
| `instagram explore` | `--limit N` | no |
| `instagram profile` | `--username <str>` | no |
| `instagram search` | `--query <str>`, `--limit N` | no |
| `instagram user` | `--username <str>`, `--limit N` | no |
| `instagram followers` | `--username <str>`, `--limit N` | no |
| `instagram following` | `--username <str>`, `--limit N` | no |
| `instagram follow` | `--username <str>` | **yes** |
| `instagram unfollow` | `--username <str>` | **yes** |
| `instagram like` | `--url <post_url>` | **yes** |
| `instagram unlike` | `--url <post_url>` | **yes** |
| `instagram comment` | `--url <post_url>`, `--text <str>` | **yes** |
| `instagram save` | `--url <post_url>` | **yes** |
| `instagram unsave` | `--url <post_url>` | **yes** |
| `instagram saved` | `--limit N` | no |
| `tiktok explore` | `--limit N` | no |
| `tiktok search` | `--query <str>`, `--limit N` | no |
| `tiktok profile` | `--username <str>` | no |
| `tiktok user` | `--username <str>`, `--limit N` | no |
| `tiktok following` | `--limit N` | no |
| `tiktok follow` | `--username <str>` | **yes** |
| `tiktok unfollow` | `--username <str>` | **yes** |
| `tiktok like` | `--url <video_url>` | **yes** |
| `tiktok unlike` | `--url <video_url>` | **yes** |
| `tiktok comment` | `--url <video_url>`, `--text <str>` | **yes** |
| `tiktok save` | `--url <video_url>` | **yes** |
| `tiktok unsave` | `--url <video_url>` | **yes** |
| `tiktok live` | `--username <str>` | no |
| `tiktok notifications` | `--limit N` | no |
| `tiktok friends` | `--limit N` | no |
| `linkedin search` | `--query <str>`, `--limit N` | no |

### Jike / Medium / Substack / Others

| Command | Args | Write? |
|---------|------|--------|
| `jike feed` | `--limit N` | no |
| `jike search` | `--query <str>`, `--limit N` | no |
| `jike create` | `--text <str>` | **yes** |
| `jike like` | `--id <post_id>` | **yes** |
| `jike comment` | `--id <post_id>`, `--text <str>` | **yes** |
| `jike repost` | `--id <post_id>`, `--text <str>` | **yes** |
| `jike notifications` | `--limit N` | no |
| `jike post` | `--id <post_id>` | no |
| `jike topic` | `--id <topic_id>`, `--limit N` | no |
| `jike user` | `--username <str>` | no |
| `medium feed` | `--limit N` | no |
| `medium search` | `--query <str>`, `--limit N` | no |
| `medium user` | `--username <str>` | no |
| `substack feed` | `--limit N` | no |
| `substack search` | `--query <str>`, `--limit N` | no |
| `substack publication` | `--name <str>`, `--limit N` | no |
| `sinablog hot` | `--limit N` | no |
| `sinablog search` | `--query <str>`, `--limit N` | no |
| `sinablog article` | `--url <article_url>` | no |
| `sinablog user` | `--id <user_id>` | no |
| `ctrip search` | `--query <str>`, `--limit N` | no |
| `reuters search` | `--query <str>`, `--limit N` (max 40) | no |
| `smzdm search` | `--keyword <str>`, `--limit N` (default 20) | no |
| `yahoo-finance quote` | `--symbol <ticker>` | no |
| `barchart quote` | `--symbol <ticker>` | no |
| `barchart options` | `--symbol <ticker>` | no |
| `barchart greeks` | `--symbol <ticker>` | no |
| `barchart flow` | `--limit N` | no |
| `grok ask` | `--text <str>` | **yes** |
| `jimeng generate` | `--prompt <str>` | **yes** |
| `jimeng history` | `--limit N` | no |
| `chaoxing assignments` | — | no |
| `chaoxing exams` | — | no |
| `weixin download` | `--url <article_url>` | no |
| `doubao status` | — | no |
| `doubao new` | — | no |
| `doubao send` | `--text <str>` | **yes** |
| `doubao read` | — | no |
| `doubao ask` | `--text <str>` | **yes** |
| `coupang search` | `--query <str>`, `--limit N` | no |
| `coupang add-to-cart` | `--id <product_id>` | **yes** |
| `yollomi generate` | `--prompt <str>` | **yes** |
| `yollomi video` | `--prompt <str>` | **yes** |
| `yollomi edit` | `--image <path>`, `--prompt <str>` | **yes** |
| `yollomi upload` | `--file <path>` | **yes** |
| `yollomi models` | — | no |
| `yollomi remove-bg` | `--image <path>` | **yes** |
| `yollomi upscale` | `--image <path>` | **yes** |
| `yollomi face-swap` | `--source <path>`, `--target <path>` | **yes** |
| `yollomi restore` | `--image <path>` | **yes** |
| `yollomi try-on` | `--person <path>`, `--garment <path>` | **yes** |
| `yollomi background` | `--image <path>`, `--prompt <str>` | **yes** |
| `yollomi object-remover` | `--image <path>` | **yes** |
