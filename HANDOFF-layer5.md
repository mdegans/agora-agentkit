# Layer 5 (review fixes) handoff

Worktree `/home/claude-agora/Projects/agentkit-display`, branch
`display/review-fixes`. Not pushed, no PR, version not bumped. This file is
uncommitted on purpose; delete it when the layer is finished.

## State

| Item | State |
|---|---|
| A (readings) | DONE, commit `4b26d83` |
| B (scheme spellings, `LinkRule` literal prefix) | DONE, `4b26d83` |
| C (`LinkRule` parsing, deny semantics) | DONE, `4b26d83` |
| E (JSON trait) | Code DONE; commit was in flight when this was written (see "Verify first"). Gaps listed under E below |
| D (parity oracle in the seed tests) | NOT STARTED |
| F (small ones) | NOT STARTED, except the `AgentDisplay` doc sentence about stateful views (in E's commit) |
| CHANGELOG `## 0.166.0 (unreleased)` | NOT STARTED for anything in layer 5 |
| TOML dev-dependency and tests (coordinator's addition) | NOT STARTED |
| `just test-slow` | NOT RUN in this layer |

### Verify first

The E commit was started as one command: `just features && just consumers &&
just wasm && git add -A && git commit …` (the pre-commit hook then runs fmt,
clippy `--all-features --all-targets -D warnings`, `cargo test
--all-features`). It was still running when this file was written. Run
`git log --oneline -3` and `git status --short`:

- If the top commit is `display: AgentJson, the JSON view as its own trait…`
  and the tree is clean but for this file: E is committed and green.
- If the top commit is still `4b26d83` and the tree is dirty: one of the
  three `just` recipes or the hook failed. The likeliest causes are a
  feature-gated type in the `agent_json![…]` list in `src/display/json.rs`
  (the trait and list are not feature-gated; the listed types must all exist
  with no features and on wasm32), or an unused import under some feature
  set. Fix, then commit with the message in "E" below.

Last results I saw myself:

- After A/B/C (`4b26d83`): pre-commit hook green; before it,
  `cargo test --all-features`: `2056 passed; 0 failed; 14 ignored` (lib),
  doctests `3 passed; 2 ignored`.
- After E's edits: `cargo clippy --all-features --all-targets -- -D warnings`
  clean; `cargo test --all-features --lib display::json`: `11 passed` after
  the last fix (the list count is 77); `cargo test --all-features --lib
  display` before the last two fixes: 107 passed, 2 failed, both fixed since.
  The full suite, `just features`, `just consumers`, `just wasm` were NOT
  seen green after E.

## Decisions already made (do not re-decide)

### A. Readings (`src/display.rs`: `every_reading`, `Run`, `skip_dropped`)

Two readings are not enough. For `raw` + deny,
`https://ok.example'@denied.example)@ok.example/x` passes both the prose
reading and the whole-token reading, and a reader that stops at `)` gets the
denied host. So **every end is checked**:

- The prose reading (`url_end`: up to ASCII whitespace or one of
  ``<>"'`()[]{}|``, then back over `.,;:!?*_`).
- One reading for each later bracket/quote, each place something was
  "dropped", and the end of the run. The run ends at the next whitespace
  that is not dropped.
- "Dropped" (`skip_dropped`) is a tab or any `threads::is_line_break`
  character (LF, CR, VT, FF, U+001C–1E, U+0085, U+2028, U+2029), **plus the
  whitespace before one in the same run**. Reason: the runner applies the
  policy and then `threads::tidy`, which turns those into `\n` and trims
  trailing whitespace, so `https:   \nevil.com` and `ht\u{2028}tps://evil.com`
  become parser-readable after the policy ran. A space followed by a word
  ends a URL.
- Closing punctuation (brackets, quotes, `.,;:!?*_`) at a reading's end is
  not part of it. So `[t](https://allowed.example)` and
  `(https://example.org/wiki)` are left. The reading that keeps the junk
  (`/wiki)`, host `allowed.example,`) is deliberately not checked; this is
  documented on `Links`.
- Exception to that: if the dots trimmed off make the last path segment `.`
  or `..` (also via `%2e`), the reading with the dots is checked too
  (`passes_dotted`). `https://en.wikipedia.org/wiki/x/../..` was allowed as
  `/wiki/` by layer 1 while a parser reads `/`.
- Under `allow`, each reading must be allowed **by some rule** (not the same
  rule): the guarantee is about destinations, and same-rule only adds false
  defanging.
- Every reading must parse and have no backslash or control character, under
  `allow` **and** `deny` (a reading WHATWG rejects can still be fetched by a
  lenient client: Python's `urlsplit` takes `ok.example<.denied.example`).
- More than `MAX_URL_ENDS` (16, public const) readings, or more than
  `MAX_URL_BYTES` to the run's end: not left.
- The "URL with another inside it is never left" rule applies to the prose
  reading only; scanning resumes at the prose end, so a start in the tail is
  judged on its own.
- Known cost, documented and tested: a bare allowed host at the end of a
  line followed by a word is defanged (`https://example.org\nNext` reads as
  host `example.orgnext`), as are `[x](https://example.org),next` and
  `<a href="https://example.org">t</a>`. A trailing slash avoids it.
- Known limits NOT fixed, not yet in docs or CHANGELOG (put them there):
  1. A cut after the policy can make a different URL: under `raw` + deny,
     `https://denied.example.ok.com/` cut after `denied.example`; under a
     wildcard allow, `https://evil.com.allowed.example/` cut after
     `evil.com`. The brief wants policy before the cut for parity.
  2. A deny rule whose path ends in sentence punctuation (`/a.`) is not
     matched when that character is trimmed as the sentence's.
  3. Whitespace other than a tab/line break ends every reading.

### B. Scheme starts (`scheme_at`, `host_follows`)

- A start is `h t t p [s] :` then any run of `/` and `\`, any case, with
  "dropped" text (above) tolerated between any of those bytes.
- `http://` / `https://` written contiguously is always a start (the old
  scanner's behaviour, `go to https://` → `hxxps://`).
- Any other spelling is a start only if a host follows (`host_follows`): a
  letter, digit, `%`, `[` or non-ASCII byte before the first `/ \ ? #` or
  space. If the first byte is `:` it needs an `@` later (`http::Request` is
  Rust and stays; `http::@host` is a URL). Lookahead is capped at 256 bytes
  (then: a start); uncapped, `http::` repeated cost 45 ms per 64 KiB.
- `https:` at the end of a line followed by a word on the next line IS a
  start (a parser drops the newline). `https: word` is prose.
- **Separator: kept as written.** Only the letters change: `http` → `hxxp`,
  lower-cased; the `:`, slashes and any dropped characters are copied. Old
  inputs give old output, and a prose false positive only loses `tt`.
- `Start.plain` is true only for exactly `http://`/`https://` not followed
  by a third `/` or `\`. Only plain starts can become `agora://` site links
  or be left as written under rules; any other spelling is always defanged
  when there is a rule.
- The default invariant is tested independently of the scanner by
  `fetchable`/`places`/`fetched` in `src/display/tests.rs` (split on
  whitespace and brackets, `Url::parse`, host must have a letter or digit or
  be an IP), also after `threads::tidy`.
- `LinkRule::from_str` refuses a string not literally starting `http://` or
  `https://` (`LinkRuleRefusal::Slashes`), also an empty authority.

### C. `LinkRule`

- New refusals: `Slashes`, `Backslash`, `Control`, `WildcardSuffix` (fewer
  than two labels after `*.`), `TrailingDots`, `DotSegment`; `Userinfo` now
  also for a bare `@` in the authority.
- Deny ports: `Url` drops a scheme's own port, so "no port" and
  "`https://h:443`" are the same rule and both deny **every port, both
  schemes**. A rule with any other port denies that port under both schemes
  (`http://h:443` is a written port: denies `https://h/`).
- Deny paths: percent-decoded, split on `/` and `\`, `;params` dropped,
  empty segments dropped, and matched both as written and with `..`
  resolved after decoding (my addition beyond the brief).
- Allow with a path also refuses `;` and `%25` in the candidate path.

### E. JSON trait (`src/display/json.rs`)

```rust
pub trait AgentJson: Serialize {
    fn json_exempt(&self) -> Exempt<'_> { Exempt::NONE }
    fn agent_json<C: AsRef<DisplayConfig>>(&self, cx: DisplayContext<'_, C>)
        -> Result<Value, serde_json::Error>;
}
```

- Not feature-gated. Generic impls: `&T`, `Box<T>`, `Option<T>`, `[T]`,
  `Vec<T>` (each item with its own exemptions), `WriteAck<I: Serialize>`.
  No blanket impl.
- `agent_json![Type, Type => EXEMPT, …]` writes each impl and, under
  `cfg(all(test, feature = "schemars"))`, `fn listed()` with each type's
  name, inline schema and exemptions. `exemptions_agree_with_the_schemas`
  (`src/display/json/tests.rs`) walks `listed()` and asserts the count (77)
  and the set of URL-typed keys.
- Hand impls outside the macro (enums that answer per variant):
  `ContentResponse`, `errors::ApiErrorBody`. They are not walked by the
  schema test; `each_content_variant_exempts_what_its_type_does` in the seed
  tests covers `ContentResponse`. `ApiErrorBody` has no test yet.
- `FOLLOWED_URL_KEYS` is removed; `BanInfoResponse`, `DataExportResponse`,
  `operator::EnrolmentResponse` got `JSON_EXEMPT` with `followed`.
- `json_exempt`/`agent_display_json` are gone from `AgentDisplay`; the three
  view-struct overrides in `prompt.rs` are gone.
- New `JSON_EXEMPT`s: `PostResponse` (`notice` verbatim, `designation`
  nested → `ActiveDesignation` `note`), nested through
  `PostWithCommentsResponse.post`, `CommentChainResponse.root`,
  `SearchResponse.results`, and `ContentResponse::Post/Comment`;
  `CouncilMeetingResponse` (`summary`, `decision_summaries`);
  `MyModerationRecord` (all); `GovernanceChainLink` (`attestation`, `data`,
  `texts`); `GovernanceSearchHit` (`title`, `tags`, `vote_tally`,
  `snippet`).
- The 77 listed types are the `agent_json![…]` invocation; read it there.

E gaps for the next agent:

- `ExportedPost.post` (`src/responses.rs` ~802) holds a `PostResponse`; it
  sits under `DataExportBundle`, which is verbatim whole, so nothing needed,
  but confirm.
- The reader's own profile fields are as written in text and cannot be
  expressed by key in JSON (`AgentResponse` is the same shape for self and
  others): report it, do not try.
- `GovernanceSearchHit` is not referenced by any other wire type in this
  crate; find what the server returns it in.
- Not listed, deliberately so far: `client::ReceivedEvent` (not
  `Serialize`), `ContactRequestId`/`CommentId`/`AppealId` (bare ids).
  `state::KeyRotation` is listed; `govlog::KeyRotation` is a different type
  and is not.
- Server answer types seen in
  `agora-raw-rest/crates/agora-server/src/mcp/handler.rs` (`ack_json(&…)` and
  `serde_json::to_string`): `StatusResponse`, `Vec<PostResponse>`,
  `ContentResponse`, `SearchResponse`, `Vec<CommunityResponse>`,
  `AgentResponse`, `GovernanceLogIndex`, `GovernanceVerification` and its
  `.summary()` (`GovernanceVerificationSummary`), `MyModerationRecord`,
  `ConstitutionResponse`, `DashboardResponse`, `PostCreated`,
  `DesignationCreated`, `ContentDeleted`, `TrashPage`, `TrashRestored`,
  `TrashErased`, `WriteAck<CommentId | FlagId | AppealId>`, `BallotReceipt`,
  `VoteResponse`, `FriendshipBatchResponse`, `FriendsResponse`,
  `SendMessageResponse` (assumed, from `send_success`; check),
  `InboxResponse`, `UnreadMessages`, `NotificationsResponse`,
  `ConfigResponse`. All of these have an impl. I did not trace
  `get_proposals`, `get_council_meetings` or `export_data` in the handler;
  `ProposalsResponse`, `CouncilMeetingResponse`, `DataExportResponse` are
  listed. I did not go through `AGENT_TOOLS` or `src/polls.rs`,
  `src/calendar.rs`, `src/git.rs`, `src/state/` type by type.

Commit message used for E:
`display: AgentJson, the JSON view as its own trait, implemented by name for each wire type`

## Not started: what remains, exactly

- **D**: `src/reactor/agent/seed/prompt/tests/display.rs` `old_server()` is
  still `Links::default().rewrite`. Paste the old scanner from
  `/tmp/claude-1001/-home-claude-agora-Projects-agora/0bfd6945-90d8-4302-a51b-878e0181a0ed/scratchpad/urls.rs`
  (`rewrite_at`, `url_end`, `on_site_host`, `matches_at`,
  `at_word_boundary`, `is_authority_byte`). Deliberate differences to pin:
  layer 1's two (scheme anywhere; URL after another's `:`/`@`), every
  `SPELLINGS` entry in `src/display/tests.rs`, and site URLs written
  `https:///…` (no longer links). Plus the four fixtures the brief lists.
- **F**: `DisplayContext::body` policy-first; poll result option names
  (`prompt.rs`, the `line` closure and `gone`/`moved` in `impl AgentDisplay
  for PollResults`); remove `DisplayContext::new` and `From<&C>` (production
  code always passes a `DisplayContext`; only tests and docs use them:
  `src/display/agent/tests.rs::a_config_is_a_context`, the doc comments in
  `agent.rs`), then `agent_display`, `agent_text`, `AgentText::new` can take
  `DisplayContext` directly; rename decided: `cx.links(text)` →
  `cx.linked(text)` (47 call sites), not done.
- **CHANGELOG**: everything. Note the existing entry still describes
  `agent_display_json`, `FOLLOWED_URL_KEYS`, "66 response types",
  `DisplayContext::new`, and the old invariants.
- **TOML**: add `toml` to `[dev-dependencies]` only. The coordinator relayed
  the owner's approval; I did not see it first-hand. Check
  `cargo tree -i openssl-sys` and `just no-openssl`.
- **Quality bar**: `just test-slow`; a full `just features`/`consumers`/
  `wasm` pass at the end.

## Traps

- The hook's test run takes minutes; `git commit` can outlast a 3-minute
  foreground call and be backgrounded. Do not start a second cargo while it
  runs.
- `has_scheme`, `scheme_at`, `authority_end` now take `&str`, not bytes
  (`skip_dropped` needs chars).
- Escaping after the policy can make the scanner's own `has_scheme` fire
  (`http:<` → `http:&lt;` has letters where a host would be). The agent test
  un-escapes and uses `fetchable` instead.
- The `www.` schemeless start is best effort (needs a host byte right after
  `www.`); the deny property test only asserts for that shape.
- `LinkRule` tests that end a string in `\t` get it trimmed by `from_str`.
- `many_urls_are_linear` takes about 20 s in a debug build now (18 shapes).
- My own attack pass (191 inputs: Unicode line breaks, HTML/markdown
  contexts, IDNA look-alike dots, dot segments across brackets and line
  breaks, site URLs with tails) found nothing through, after the fixes
  above; the two things it led to are the `tidy` interaction and the
  trimmed dot segments.
