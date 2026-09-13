# DeepSeek search, measured

Whether flint can give every model — including a local one — a working web search, and what
that costs. **This is implemented**: `src/search.rs`, and §4 is the design it was built to. This file exists because the answer is not what the obvious reading of DeepSeek's
documentation says, and because the measurements here took a key and a few minutes that should
not have to be spent twice.

Everything below is `VERIFIED` on 2026-09-13 by making real requests to
`https://api.deepseek.com/anthropic/v1/messages` with a live key. The two responses are
described in §2 and §3; nothing here is reasoning about what would happen.

## 1. The mechanism: server-side search on the Anthropic surface

```
POST https://api.deepseek.com/anthropic/v1/messages
x-api-key: <the key already in config.toml for the DeepSeek provider>
anthropic-version: 2023-06-01

{
  "model": "deepseek-flash",
  "max_tokens": 2048,
  "messages": [{ "role": "user", "content": "..." }],
  "tools": [{ "type": "web_search_20250305", "name": "web_search", "max_uses": 1 }]
}
```

DeepSeek runs the search **on its own servers, inside its own model turn**, and returns the
results as structured blocks. Nothing about this depends on which model is driving flint: the
search is a tool call, so a local model can ask for it and read the answer.

### The endpoint is not the one in `config.toml`

`base_url` for a DeepSeek provider is `https://api.deepseek.com/v1` — the chat-completions
surface. **That one does not search.** DeepSeek's Responses API compatibility table lists
`web_search` under built-in tools as **Ignored**, and its Anthropic table lists
`server_tool_use` and `web_search_tool_result` as **Supported** (`DOCUMENTED`, both read from
`api-docs.deepseek.com` on the same day). So the search endpoint is a separate address that
has to be written down separately:

> 搜索端点使用 Anthropic 兼容基址（`https://api.deepseek.com/anthropic/v1`），不同于 LLM 适配器使用的
> chat-completions 基址——**绝不复用 `$DEEPSEEK_BASE_URL`**。 — `@deepseek-ai/dsh-web-search-deepseek`

That warning is DSH's, and it is right. Reusing the provider's base URL silently produces a
request that is answered without any search at all.

## 2. What comes back

A response to a one-search prompt had these content blocks, in order:

```
["thinking", "text", "server_tool_use", "web_search_tool_result", "thinking", "text"]
```

- `server_tool_use` carries the query DeepSeek chose: `{"query": "latest stable Rust release"}`.
  It wrote its own query; the caller's words are not the query.
- `web_search_tool_result.content[]` held **10 items**, each with keys
  `encrypted_content`, `page_age`, `title`, `type`, `url`. Ten results per search, and
  `page_age` is often absent entirely.
- **A failed search is an item too**, with keys `["error_code", "type"]` and no url. A reader
  that assumes every item is a result will produce an entry with `url: null`. DSH skips
  anything whose `type` is not `web_search_result`, which is the right rule.

### Snippets do not exist

DSH builds each result's `snippet` from `citations[].cited_text` on the response's `text`
blocks. **DeepSeek returned zero citations**, and its own compatibility table lists
`citations` as **Ignored** for `text` content. So that path yields nothing here, and a search
tool built on it returns `url` and `title` only.

The prose is where the citations actually are, as ordinary markdown links:

```
... is **Rust 1.98.0**, which entered the stable channel in late August 2026
([Rust 1.98.0 正式发布, 2026-08-24](https://xuanwu.openatom.org/articles/news/260824.html))
```

So the choice a tool has to make is: return the source list alone (DSH's answer — it discards
the prose, on the grounds that a provider's text is not the answer), or return the prose as
well, labelled as what it is. For a local model the second is much more useful and the first
is nearly useless: ten titles and no content means ten `fetch` calls before anything is
learned. The label has to be honest, because the prose is a model's summary of pages nobody
has read.

### URLs carry a fragment

Some results came back as `https://36kr.com/p/3596665747407107#1`. The `#1` is DeepSeek's, not
the page's. Anything that dedupes or displays these should strip the fragment, or two results
for one page look like two pages.

## 3. What it costs

This is the part that changes a design, and it is not in the documentation.

| Call | `max_uses` | searches DeepSeek reported | `input_tokens` | `output_tokens` |
|---|---|---|---|---|
| 1 | 1 | 1 | 16,561 | 336 |
| 2 | 1 | **2** | **119,581** | 644 |

`usage.server_tool_use.web_search_requests` is DeepSeek's own count, and it is the number to
believe over `max_uses`: the second call was asked for one use and made two. Whatever
`max_uses` limits, it is not "one query".

**A search can cost six figures of input tokens**, because the engine's pages go into the
context of the model turn that is doing the searching. The first call's 16K is the floor and
the second's 120K is what it looks like when the model decides it needs more. A tool that
offers search has to say so: this is not a cheap call, and a model that treats it like `grep`
will spend a great deal of the user's money being unhelpful.

The output tokens are small (336, 644) — the model behind the search is doing retrieval, not
writing an essay, even though it does produce a summary.

## 4. What this means for flint

**Search is a tool, and every model gets the same one.** Not a provider feature, not a
capability of the driving model: a call flint makes and formats. Which is what makes the
combination the user actually wants — a local model for the loop, a DeepSeek key for the
search — work, and work without deploying anything. No SearXNG, no container, no second
service, on any machine that has the key.

**It is a self-contained HTTP call.** The request above is Anthropic-shaped and the
session loop is chat-completions-shaped, and they never meet: the tool makes one request,
parses one response, and returns text. No streaming, no tool-call plumbing, no second client
in the agent loop. Roughly a hundred lines with `reqwest` and `serde_json`, which are already
dependencies.

**It is a separate configuration block.** The key can be reused from an existing
`[[providers]]` entry, but the address cannot: `[search] base_url` is
`https://api.deepseek.com/anthropic/v1`, and a search tool whose endpoint came from the
active provider would break the moment the active provider was a local one — which is the
case this exists for.

**A tool that is offered only when it is configured.** No `[search]` block, no `search` tool:
the same rule as `skill`, which is not registered when there are no skills. Then the prompt
says to use `fetch` with a URL the model already knows, which is what a model without search
can honestly do.

**Cost belongs in the tool description**, because the model is the thing that decides to
call it. One sentence saying a search is a full model turn with tens of thousands of input
tokens is the difference between a tool that is used and a tool that is abused.

## 5. What was built from this

`src/search.rs` is the implementation and §4 is the design it follows. The credential is
inherited from a DeepSeek provider when there is one and named explicitly in `[search]` when
there is not; the tool is registered only when it can actually work, and the reason is said
once at startup when it cannot.

One thing the first live run through the finished tool added: **the summary can be wrong.** It
claimed Rust 1.97.1; the model cross-checked against `rustc --version` and `endoflife.date`,
found 1.98.1, and said which of its sources was stale. That is the labelling in §2 doing its
job, and it is the reason the summary is returned with its sources rather than on its own.

## 6. Still not measured

- Whether `max_uses` has any effect at all, or what it counts.
- How often the summary is stale. One run out of one was.
- Whether a second search provider is worth having. Brave or a self-hosted SearXNG would be
  cheap per call where this is expensive, and would work without a DeepSeek key — but neither
  is needed for the case this file was written for, and neither is free of deployment.
- What the search returns for a Chinese-language query, or a query about a private codebase.
- Whether `deepseek-v4-flash` differs from `deepseek-flash` here. Only the latter was tried.
- Whether the `encrypted_content` field on each result is usable for anything. It was not
  touched; it is presumably what lets a later request refer to a result without re-sending it.
