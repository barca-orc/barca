# Changelog

Release notes for barca, newest first: each `## <version> (<date>)` section is published as that
version's [GitHub release](https://github.com/barca-orc/barca/releases) body, and changes not
released yet collect under `## Pending`. Releases 0.1.2 to 0.23.x have their notes only on
GitHub Releases, apart from the upgrade notes kept below for 0.10.0 to 0.20.0; the "0.1.2 to
0.8.0" summary below lists what the earlier of them shipped.

## Pending

- UI-only PRs and merge groups run UI checks and browser tests without unrelated backend suites; releases keep full coverage.

### Documentation

- **`barca docs hashing`, a new manual topic (and site page), holds the full rules for a run hash's definition part:** which project code is followed from a step's function, which decorator arguments count, how a module name is resolved, what is followed conservatively and what is not followed. `barca docs cache` keeps the summary and the helper-module example and points to it, so The Cache page is about a fifth shorter.
- **barca.sh addresses match their names.** Recipes are under `/recipes/` and worked examples under `/examples/`, with readable addresses (`/recipes/retries-timeouts-failures/`, `/examples/partitions/`) in place of `/patterns/06-error-handling/` and `/workflows/03-parametrized-assets-and-partitions/`, and Pausing Assets is `/reference/pausing/`. Every old address redirects (301) to the new one.
- **barca.sh is organized by what you are doing.** The sidebar groups pages as Start Here, Why Barca, Writing Pipelines, Running and Caching, Scheduling and Servers, Storage and Configuration, Automation and Agents, and Recipes (with the worked examples inside), where Reference was one list of 30 pages. Architecture pages are under Internals, and contributor pages and RFCs under Project, both collapsed. `/reference/` lists every page A to Z with what it covers, and `/patterns/`, `/workflows/`, `/comparisons/`, `/rfcs/` and `/contributing/` are index pages instead of 404s. RFCs rank below the reference in search and open with a note that they are design records. "Edit page" links no longer 404. No page moved.
- **barca.sh's homepage says who barca is for and shows it working.** The hero pairs the pitch (cached Python pipelines with no platform to run) with a two-asset pipeline and its two runs, the second running 0 steps; measured numbers against Dagster link to the comparison; "Where to go next" sends newcomers, evaluators, pipeline authors, operators and agent users to their page. Every page's header links to Docs, Recipes, Reference and the Changelog, and inline code that wraps on a phone keeps its background on both lines.
- **Three diagrams on barca.sh:** a run from parse to record, with where the cache is checked (Architecture); partitions fanning out per key and collected, with what reruns when a key is added (Guide); and a team setup of laptops, `barca serve` and a bucket, with what each sends where (Share Results With a Team). They are inline SVG and follow the light and dark themes.
- **The longest reference pages open with a summary.** `barca docs cache` (and the Cache page) starts with an "At a glance" table of what runs again after each kind of change, including what barca cannot see (a file read in the function body, an environment variable not listed in `env=`). The CLI reference starts with a table of common tasks and the command for each.
- **barca.sh: one name per page, and answers in the reader's words.** Sidebar entries now use the page's own title, so the sidebar, search, the A–Z list and the page agree (recipes are titled by what they do, such as "Retries, Timeouts and Failures"; worked examples start "Worked Example:"). A new page, "Why Did (or Didn't) It Run?", answers the common caching surprises (a changed file or environment variable that reran nothing, a downstream that reran on identical bytes, a task that always runs) and how to ask barca why. RFCs are left out of the sidebar and search, and are listed at `/rfcs/`. Scheduling is "Scheduling With Cron" and Telemetry is "Telemetry and Slack Alerts", so those searches find them. The search box sits in the same place on every page, phones get a Docs link on the homepage, and the homepage's code sample wraps instead of being cut off. The sidebar shows only the group holding the current page open and scrolls to that page, diagrams are drawn top to bottom on phones, the CLI reference groups its commands under "Commands in detail" and opens "Errors and exit codes" with a table of the codes, and the A–Z list has letter headings and a jump bar.
- **The CLI reference is one page per command.** `/reference/cli/` keeps common tasks, command shape, output format, exit codes and a table of every command; `barca get`, `barca run`, `barca serve` and the others have their own pages under `/reference/cli/`, listed A to Z under Commands in the sidebar together with `status`, `sql`, `pin` and `unpin`, `logs` and `doctor`, whose reference pages cover them. The sidebar also links straight to "Forcing a Recompute" and "Slack Alerts", and the homepage's cards carry the titles of the pages they open. Each command page opens with its usage and flags, and the manual-topic note moved from the top of each reference page to a line at its foot. `barca docs cache` (and The Cache page) puts forcing a recompute and `--dry-run` right after what is cached, and the incremental-run internals near the end. The header links Get Started, Recipes, CLI, All Pages and the Changelog, each marked current only on its own pages; sidebar shortcuts such as Exit Codes light up when their section is open; Getting Started opens with what the page covers; light-mode rules are a soft grey and the phone menu button is an outlined square. Links to the old sections point at the new pages. Exit codes have their own sidebar entry under CLI Reference.
- **`barca docs hashing` opens with a three-line summary** (the function and the project code it uses; which decorator arguments count; what is not followed) and puts the edge cases, a helper that does not parse and how the pipeline file is named, last. barca.sh's homepage has the menu on phones, Recipes and Worked Examples list their pages in reading order, and long commands in tables wrap instead of squeezing the table.
- **"Running barca serve" (`/serve/`)** opens Scheduling and Servers: what the server does, how to start it, and a table of which server page answers which question.
- **`barca docs hashing` opens with a three-line summary** (the function and the project code it uses; which decorator arguments count; what is not followed) and puts the edge cases, a helper that does not parse and how the pipeline file is named, last. barca.sh's homepage has the menu on phones, Recipes and Worked Examples list their pages in reading order, and long commands in tables wrap instead of squeezing the table. The homepage heading says what barca is; Deploying barca serve summarizes restarts and memory limits and links to Serve Operations instead of repeating it; the run-hash page is "The Run Hash: Functions, Helpers and Decorator Arguments".
- **barca.sh checked against barca 0.42.0.** An audit ran the site's examples and compared its claims with the code. Pages no longer say `barca serve` has no authentication (opt-in project tokens shipped in 0.36.0); queued runs are documented as resuming after a restart; the Server API has no 10-minute run limit (there is none by default); the configuration example parses; `barca pause`, `resume` and `tokens` are in the CLI reference; flags, output samples, `/health` and `POST /runs` fields, the architecture's database (libSQL), Datadog's one trace per step, RFC statuses and the worked examples' claims (`--refresh` of a key, `name=` in status and SQL, task output, retry attempts) match 0.42.0. A new page, The Web UI, covers its states, run recovery, cancelling, the key explorer, incidents and tokens.

## 0.42.0 (2026-11-09)

### Features

- Cancel queued or active runs from their inspector, with confirmation and cancellation progress. Incomplete runs no longer claim unrecorded steps were cached or successful. Resume a paused pipeline directly from its Incident controls, with the affected file and count confirmed before resuming. Pauses in other pipelines remain in place.

- Retry exactly the partition keys shown or grouped under one issue, including scattered multi-dimensional combinations. `--partition` also accepts a JSON array of exact key objects; queued runs retain that scope across restarts.

- **Repair individual partition keys:** upload replacement data or pin an earlier version from the key inspector or `barca pin 'asset[dimension=value]'`. Pins hold only the named key through refresh, identify held keys in the explorer, and show the held version in previews. Confirmations accept a repair reason. Unpinning restores normal computation.

- **Web UI: calmer, less crowded layout.** Four type sizes (12/13/15/20px, 13 body) with weight for hierarchy and sentence-case column headers. Each page has one toolbar row (title and count, the state filter, search, the last-run filter, the List/Graph switch and Run); the "1 failing · blocks N downstream" banner is gone (the failing filter's tooltip says what it blocks). Asset, task and sensor rows are one 36px line under a foldable header per source file (its count and worst state); the path under each name is gone (the id is in the tooltip), Last run and Last success are one cell (`17m ago`, `failed 17m ago · last ok 2d ago`), Next run shows only when a node has a schedule and Typical only when a node takes 0.1s or more. The top bar no longer has a Run button: the page toolbar runs what is open (`Run all`, `Run billing.py`, the count in its tooltip), the node panel or graph inspector runs its node, and Ctrl/Cmd-K offers both. The side panel is 400px with plain sections instead of cards, a one-line title and id with the copy button inline, and a failure's error shown once (the asset panel's Last attempt and History, and the run panel's Run and Steps sections, no longer repeat it). Runs, Tasks, Sensors and Schedules follow the same rules.
- **Web UI: one navigation, in the sidebar.** The top tab bar and top bar are gone: the sidebar holds Search (Ctrl/Cmd-K), then Assets, Runs, Schedules, Tasks and Sensors with icons and muted counts (the open one on a neutral fill, runs in progress beside Runs), then the pipelines, then Docs, the connection, the time zone and the theme; collapsed, it keeps every icon. Each page's toolbar is its first line. A pipeline's count and glyph in the sidebar are now derived exactly as its file's header in the table (they could disagree, e.g. a file counting its tasks and sensors and showing a failed task's red glyph on Assets) and are of the open page's kind, labelled (`assets`, `tasks`, `sensors`). The asset search no longer takes focus on load. The graph fits the whole pipeline with 48px of room when it fits at 0.8 zoom or more, cards are one line (glyph, name, a task's or sensor's kind; the state's word is in the tooltip), edges are a neutral hairline that the selected or hovered node's edges strengthen, and the legend lists only the states on screen. Runs shows the last 24 hours as a full-width hourly chart with a one-line summary, and a run's facts are a two-column grid without a second status badge. Empty Tasks, Sensors, Schedules and Runs pages say what the kind is and how to add one; light-theme hairlines are 5/8/13% ink like the dark theme's.
- **Web UI: one toolbar for List and Graph, and quieter panels.** The graph view has the list's toolbar (count, state filter, search, last-run filter): a node the filter leaves out is dimmed, not removed. Zoom, fit and the layout direction are one row of controls at the canvas's bottom right, and a graph smaller than the canvas is placed a little above the middle instead of leaving a band of empty space over it. Run all is gone from pages with nothing of their kind (no tasks, sensors, schedules or assets). The run panel's actions are one row: Re-run failed (the failed steps and what is downstream of them; its tooltip says so), Re-run all, and copy incident summary as an icon; the log tools' Copy and Download are icons too, so they stay on one line. Node and step names are sans everywhere; mono is kept for run ids, hashes, file:line, code and logs. The asset panel and the graph inspector share one header (the name, its kind in muted words, the id muted with its copy button, Run), and sections with nothing to say collapse: no duration histogram placeholder before two successful runs, no chart or p95 for a single run, one line for a source node's inputs and for an empty blast radius, no Consumers section when nothing reads the node, Refresh and Upload replacement… at the right of their section's title. Every toolbar control is 28px, so the title sits on the same line on every page.
- **Web UI: one control style in the asset panel.** Incident controls are one row of the same quiet button (the confirmation still warns), and say `paused` only when the asset is; links inside text (open the run, All paused assets…) have a faint underline so they read as links; a version shows `latest`, `pinned`, `evicted` or `uploaded` as a small tag beside its hash and its details on one line, cut short rather than wrapped; the Consumers section is gone (Blast radius lists everything downstream; the direct readers are under Technical details); the state's explanation starts with a capital; and the time filter's chevron matches the other controls.
- **Every key of a partitioned asset in one request** (experimental, `barca docs server-mode`): `GET /assets/{name}/partitions/grid` answers with the dimensions and their values and, for every key, its state as `barca status` decides it (fresh, stale, never run, failed, or blocked behind the same key of a failed input) and its latest attempt's time, duration, size, run and error, in compact columns (about 230 KB for 10,000 keys, computed in under half a second and kept until the database changes). It decides the state across the asset's chain, the partitioned assets it shares its keys with, so the 83 keys of a 10,000-key downstream asset that wait on 83 failed keys read as blocked and the other 9,917 as fresh. `GET /assets/{name}/partitions/key?key=` answers with one key's state in each asset of the chain, its latest attempts, and the schema and first rows of its result and of the same key of each input. Failed keys' errors are grouped with each key value replaced by its dimension's name (`s3://raw/<region>/<day>.csv`), only where it is a whole word.
- **Web UI: a key explorer for partitioned assets** (`#/keys?node=<asset>`, `barca docs partitions`, "Exploring keys in the web UI"). Every key at once on a canvas, coloured by state, one block per value of the shorter dimension with each row's first value beside it (a day × region asset of 10,000 keys is four blocks of 2,500 days, and a bad week in one region is a red streak); filters by state, dimension value, error group and key text over a virtualized table of the keys they keep; one key's detail with its error, attempts, the schema and first rows of its result and of the same key of each input, and its state in every asset of its chain; and Run for the keys that are not fresh, of the values selected or of a Shift-click range. The asset panel links to it, its grouped errors open it filtered, and an asset list row says `6 of 360 keys failed` where it said only `failing`, and `83 of 10,000 keys wait on account_scores` where it said only `blocked`. ⌘K's key results open the explorer at the key. A partitioned asset's panel no longer says "No successful run yet" and "No successful materialization" when thousands of its keys succeeded, and the toolbar has one `blocked` chip where it could show two.
- **Web UI: partitioned runs and graphs at a glance.** A run's failed keys of one asset that failed the same way are one line in its summary (`account_scores · 83 keys · open in the key explorer`, with their error) where it listed five of them and "and 78 more below". A partitioned asset's graph node says how many keys it has (`10k keys`) and draws a thin bar of the share failed and waiting to run, with the counts on hover.
- **Web UI: the key explorer says what is wrong first, and never scrolls.** It reads top down: the keys counted by state; one line per problem (each error of the failed keys, then blocked, stale and never-run keys) with how many keys and where they are (`day 2023-03-01 → 2023-03-20 · region apac`), Show and Run; an overview of every key in a few rows of columns that fit the page, fresh quiet and problems loud, that zooms to a stretch you drag across; then the keys that need attention (or all) in a table that scrolls on its own. A key's detail leads with its error, then the key along its chain and what it read. The asset panel is in tabs (Overview, Data, History, Incident) under its state, so it fits a window; its Keys section names the failed keys' errors and links to the explorer instead of listing keys 100 at a time.

### Bug Fixes

- **Recovery actions together:** replacement upload and version pinning now live in the asset’s Incident tab beside pause/resume. The summary links to pin review instead of unpinning without confirmation.

- **Readable grouped tables and explicit graph fit:** mixed group/node tables preserve column widths and scroll instead of overlapping values. Fit to screen shows every graph node, including on laptop windows, and a delayed automatic fit no longer overrides the operator’s view.
- **Partition recovery feedback:** the key explorer’s primary run follows its visible filters without widening an inexact selection; completed retries report their outcome and refresh the open key. Retained results on failed or stale keys are labeled as the last successful output.

- Web UI: the graph fits once its nodes are measured and re-fits when the canvas changes size until you pan or zoom; switching from List to Graph could leave it unfitted at the canvas's top-left corner.

### Documentation

- barca.sh: attach the Worker custom domains in `site/wrangler.jsonc` and build the site in CI's fast tier so docs deploy fixes are validated without touching the release workflow.

## 0.41.0 (2026-11-08)

### Breaking

- Web UI: a node's state reads `fresh` where it read `cached`, and a node whose last attempt failed is `failing` everywhere, never also `never run` or `cached`; the state filters follow (a `state=never_run` deep link no longer lists failing nodes, which have their own filter). The CLI and JSON keep the `CacheState` values.

### Features

- Pin an asset to an earlier materialization for crisis data fixes (experimental, `barca docs pins`): `barca pin <asset> --previous` / `--run-hash <h>` (with `--versions` to choose and `--reason`), `barca unpin <asset>`, and `barca pin` to list pins. While pinned, every run (downstream steps, `barca get`, `barca.get()`, `--server` runs, scheduled ticks) reads the pinned result as step reason `pinned` and none recomputes the asset, not even `--refresh`; the result is never evicted. The pin, with when, who and why, is a new `asset_pins` table in the metadata database. `barca serve` adds `GET /pins`, `GET /assets/{name}/versions`, `POST` and `DELETE /assets/{name}/pin` (OpenAPI specification 19), and the asset panel of the web UI a Versions list with "Roll back to this" and a pinned banner with "Unpin", each behind a confirmation. Only unpartitioned assets; uploading a file as the pinned value is not supported yet.
- **The Runs page is a debugging and recovery surface.** A run's panel opens with what failed (each failed step, its error and where it raised) and its actions: "Re-run failed steps + downstream" (the same target cache-aware: steps that succeeded are cached), "Re-run whole run" (`refresh_all`), "Retry" on each failed step or partition key, and "Copy incident summary" (markdown with the run's link, target, trigger, timing and failed steps). The new run is selected at once. The logs filter by step (a step's "Logs" button or its name on a line), by text with matches highlighted, and to errors only; "First error" jumps to it, and the lines shown copy or download. All through the existing `POST /runs` and `GET /runs/{run_id}`; no API change.
- Web UI: one state per node, in plain words. A node whose last attempt failed is `failing` (red, with "last good 3h ago" or "never succeeded"); otherwise `fresh`, `stale`, `never run` and the other cache states, never a snake_case enum; filters and header counts use the same states, and every state badge has an icon beside its colour. Assets gets a failing summary ("1 failing · blocks 2 downstream nodes") and tinted failing rows; the asset and run tables no longer scroll sideways (low-priority columns leave as the table narrows; "Typical" is now "Typical duration"), and an inline error opens the side panel instead of expanding in the row. Errors read in the sans font with the traceback below in mono, its paths relative to the project root and wrapping at `/`. The Run button is on every tab, the connection reads `Connected`, Search has a visible label, counts are pluralized (`1 run`), the run panel's markup is valid HTML, and the light theme's faint text meets WCAG AA contrast.
- `GET /assets/{name}` has `failure` when the node's latest execution failed: `raised_at` (the last traceback frame inside the project, relative to its root), the failed attempt's `run_hash` and `run_id`, and `last_success` (`null` when it never succeeded). The asset panel's "Why it failed" box shows "raised at assets.py:136" beside the definition line (now labeled "defined at") and what changed since the last success, by run hash: the components that moved, the same run hash as the last success (the cause is outside the definition), or that the code moved on after the attempt.
- Contain an incident from the asset you are looking at. `barca pause <target> --server` (a node, `--downstream` for everything below it, or a `file.py` for every node of it) and `barca resume <target>` / `barca resume --all`, also `GET /pauses`, `POST /pauses` and `POST /pauses/resume`, hold assets back from scheduled and sensor-driven ticks until resumed: a paused target is a skipped step with reason `paused` (its detail names `barca resume`), and a paused node upstream of a tick's target is read as it is. Pauses are kept in the metadata DB (`asset_pauses`) across restarts. A run by hand that names a paused node needs `--allow-paused` (`allow_paused` in `POST /runs`, a confirmation in the web UI). The web UI's asset panel gains a Blast radius section (every downstream node, and the schedules that compute the asset or what reads it, with their next run) and Incident controls (pause the asset and downstream, its pipeline file, the related schedules and the run queue, and resume each, destructive ones behind a confirmation); the assets list labels paused assets and a banner says how many are paused, linking to them (`?paused=1`). Experimental; `barca docs incidents`.
- Web UI: a node a failing node upstream keeps from running reads `blocked` (amber, with a barred-circle icon) instead of `never run` or `stale`, with "by broken_report" linking to the failure, in the Assets list, the asset panel and the graph. The Assets header has a `blocked` chip (`?blocked=1`), and "blocks N downstream nodes" counts exactly those rows. Derived in the UI from the graph; the API is unchanged.
- Pin an asset to data you supply (experimental, `barca docs pins`): `barca pin <asset> --from-file <path> [--reason ...]` (also `--server`) loads a `.json`, `.parquet`, `.csv` or `.pkl` file with the reader runs use, stores it as a new artifact of the asset with a content-derived run hash, records it as a materialization marked as an upload (a new `asset_uploads` table: when, who, file name, reason) and pins it, with the semantics of any pin until `barca unpin`. `barca serve` adds `POST /assets/{name}/pin/upload` (the file as the body, `?file_name=`/`?format=`/`?reason=`, at most 256 MiB, `403` read-only); versions gain an `upload` key; the asset panel's Versions section offers "Upload replacement…" behind a confirmation naming the file and size, and the pinned banner names who uploaded the override. Only unpartitioned assets.
- Web UI: a pinned asset reads `pinned` (warning tone, pin icon) instead of `stale`, with "override from fix.json" or "rolled back to <run>" and who pinned it, in the Assets list, the asset panel ("Pinned: runs read this version until it is unpinned", with Unpin) and the graph; it outranks every state but `failing`, and a failure above it no longer marks what reads it `blocked`. The Assets header has a `pinned` chip (`?pinned=1`) and every page a banner "N assets pinned · runs read pinned versions" linking to them. `GET /pins` (and `barca pin --json`) entries gain `run_id` and `upload`.
- **View a step's Datadog trace from the Runs page.** A run traced with `BARCA_TELEMETRY=datadog` is recorded as traced (`runs.trace`), and each step that ran or failed gets `trace_id` (128-bit, 32 hex digits) and `span_id` in `barca history RUN_ID --json` and `GET /runs/{run_id}`, derived from the run and step as the trace was, so nothing changes in the worker protocol. `GET /project` names the Datadog web app in `telemetry.datadog_url`, from `DD_SITE`, else the new `[telemetry] datadog_site` in `barca.toml`, else `datadoghq.com` when the integration is on. The run panel shows "View trace" on each step row and failed step, and "Copy incident summary" links each failed step's trace; both are hidden without a site or a trace id. Experimental; `barca docs telemetry`.
- **Web UI: a calm, neutral palette; colour only on a state's glyph.** Both themes move to true neutral greys (the dark theme loses its green cast, the light theme its grey-green), with hairline borders and one muted indigo accent for focus and pinned assets. Every state is drawn by one 14px `StatusGlyph`, its own shape and a desaturated hue tuned per theme (failing a red cross, running a turning blue arc, fresh a green check, stale amber half-filled, blocked an orange barred ring, partial violet, unsynced teal, pinned indigo; never run, unknown, always runs, paused, queued, cancelled and skipped in grey), with its plain word beside it in the text colour. The glyph replaces the coloured pills, dots and icons in the Assets list, its header filters (now one neutral segmented control), the asset panel, the graph (whose nodes lose their coloured edge bar, duplicate dot and per-node kind icon), its legend (now telling blocked, stale, partial and unsynced apart), the Runs list and run panel, the Tasks, Sensors, Schedules and grouped pages and the sidebar. Failing rows are no longer tinted or barred, the failing summary and the error boxes are neutral, and error text reads in the text colour; a failure is found by its glyph and the default sort. Every text colour and state hue holds WCAG AA contrast (4.5:1) on every background of both themes, which `tokens.test.ts` now checks.

### Bug Fixes

- Web UI: a failed run's step count no longer reads "8 ran · 0 cached": the runs list and run panel say "7 ok · 1 failed · 0 cached" (and the skipped nodes, in the run panel). `barca get --json`'s `steps_executed` is unchanged (it counts executed steps, failed ones included, as the manual now says).
- Web UI: the asset panel's History for a node whose every attempt failed says so ("No successful run yet") and lists the attempts with their errors, instead of empty duration tiles and a full-height placeholder bar.
- Web UI: the asset panel's Blast radius names each downstream node's state in the assets list's words (`blocked`, `pinned`, `failing`), not its raw cache state (`never run`).
- Web UI: the copied incident summary links to the run through the UI's hash route (`…/ui/#/runs?run=<id>`); the link it gave before opened the Assets page.

## 0.40.0 (2026-11-05)

### Breaking

- Datadog: a step execution is the unit of a trace, not the run (#845). Every step that ran or failed is a trace of its own, rooted at `barca.step` (resource the asset's short name, the partition key in `barca.partition` and `barca.node_id`), with the worker's `barca.execute` and library spans inside it; the run is a separate one-span `barca.run` trace, and a run's traces share `barca.run_id`. Gone with the run-shaped trace: the `barca.partition` operation (a key is a `barca.step` of its own), the `barca.partitions.*` metrics and the 200-keys-per-node / 2,000-per-run caps (every key has a trace), the `barca.cached` tag and the zero-length spans of cached and skipped steps (they send nothing; `barca.steps.cached` and `.skipped` on the run span count them), and the per-node summing of `barca.bytes`, `barca.cpu_seconds` and `barca.max_rss_bytes`. Monitors and trace metrics on `barca.step` per `resource` and on `barca.run` keep working; `barca.attempts` is now on every step that ran or failed, keys included. `BARCA_LOG=json` lines about a step carry the step trace's ids.

### Features

- Datadog: a step's trace is sent the moment the step ends, in the background, instead of the whole run's at the end: a failure is in Datadog seconds after it happens, a run that is killed has sent every step that finished, and the coordinator holds no spans across the run. The run's trace goes at the end as before, and sends any step the live path could not. A delivery warning names what was not received (`did not receive step pipeline.py:orders`); it is still printed once per failure streak.

## 0.39.6 (2026-11-05)

### Features

- The Python stubs are typed: `asset`, `sensor`, `task` and `sink` have overloads for bare and parameterized use, `parallel`, `parallel_map`, `collect` and `Group` carry parameter and return types, so an IDE completes and checks decorator arguments without importing barca's runtime.

### Documentation

- The manual's long topics are split, each new topic with its own page on the site: `barca docs disk` (the size limit and eviction, from `cache`), `barca docs layout` (the project root, `.barca/`, concurrent and killed runs, upgrading, from `cache`), `barca docs backup` (metadata backups and restore, from `remote` and `server-mode`), `barca docs serve-ops` (the run queue, stopping, restarting, stop grace, shared capacity, run timeout, from `scheduling`) and `barca docs batched-partitions` (`batch=`, from `partitions`). `barca docs agents` points at the contract for the output envelope instead of repeating it.
- One owner per table: the exit codes are the generated table in `barca docs contract` (agents, the skill, the README and the site CLI page point there); the cache states and reasons are `barca docs status` (the site CLI page said `unsynced` had one reason and `unknown` one; both have four); the `.barca/` entries are `specs/interfaces/dot-barca-layout.md` (the manual and the configuration page had drifted: no lock files, no `.csv`, "SQLite" for libSQL). The experimental list in `barca docs contract` and the README's command table are generated from the CLI definition and `EXPERIMENTAL`, so they can no longer disagree with the stability column (they did: tokens, `history RUN_ID` and its filters, `--reconcile`, `list --groups`, `get`/`run --batch` were missing, and the README called `state backups` and `restore` experimental).
- The site mirrors the manual topics `agents`, `assets`, `cache`, `logs`, `partitions`, `sinks`, `status`, `tasks` and `types` as generated pages with an edit link, like the pages it already generated; the development page describes the worktree and fast-profile workflow, `scripts/check-fast.sh` and the merge queue. The server API page says how `Last-Event-ID` resume works (shipped in 0.36.0) instead of calling it future work. DIRECTION.md records 0.39.5 as shipped and the 0.39.6 cleanup list; its drain note gives the exit codes the server has (0 after the timeout requeues runs, 1 only when a second signal cancels them). The specifications say the sequential historical-selection fix shipped (0.34.0) and that a server's shared in-flight steps pass their conformance scenario, close out the architecture plan with what shipped where, and record the `CacheState` enum as the one spelling of a node's state. GLOSSARY.md gains the user vocabulary (the eight states, run hash, ledger, store, environment, cutoff, node, step, attempt, batch).
- CI's fast tier now runs the prek hooks on every pull request (ruff and ruff format at their pinned release, ty, `cargo fmt --check`, the version sync and `uv lock --check`), the same hooks `scripts/check-fast.sh` runs locally; the pins move to ruff 0.14.14 and ty 0.0.97, also pinned in pyproject.toml's `dev` dependency group. The release process refreshes `scripts/ci-test-durations.json` from the full tier's measurements (`scripts/release.sh`, the `durations` job), Dependabot proposes weekly grouped updates for Cargo, uv, the two npm trees and the actions, and `scripts/new-worktree.sh --linker` copies the trunk's linker settings into a new tree. `tests/README.md` describes the shell integration and simulation suites.

### Bug Fixes

- The server's `GET /runs/{run_id}/output` reads a final artifact's format from the same extension table as the worker and `barca sql` (`crates/barca-core/src/artifact_format.rs`, mirrored from `barca._artifacts.FORMAT_OF_EXTENSION` by a test): it took every extension other than `.json`, `.parquet` and `.csv` for pickle, where `barca sql` reports `unknown`. The `Content-Type` it serves is unchanged (pickle and unknown are both `application/octet-stream`); the two tables can no longer disagree.
- UI: every node state is one of the eight words `barca status --json` prints (`cached`, `stale`, `unsynced`, `blocked`, `never_run`, `partial`, `always_runs`, `unknown`), shown as the server says it with its `reason` verbatim; the UI no longer relabels them (no "Healthy", "Not run", "Stale (some keys)") or rewrites a state from the last run: a failed last attempt is shown as its own fact (`last attempt failed`) beside the state, `blocked` is not folded into `stale`, and a task stays `always_runs`. Health bars, group summaries, the graph legend, node colours, the sidebar dots and the state filters all come from one table keyed by the generated `CacheState`, so a state the server adds cannot be left out. Failure mutes (this browser's `localStorage`), the demo "Preview failed check" and the placeholder asset graph shown before the server answered are gone: the UI shows exactly what the server reports, and a loading state until it does.
- UI: the Playwright suites gate CI again (#807). Of the seven specs failing on main, six were stale after #753, #776 and #777 (`.barca-run-steps`, `executed`/`cached` counts, `21,950 / 28,404`, "Retrying" status, "Preview failed check", "Muted · 1") and one was the node panel crashing on a node definition without a `file` (stub data only; the panel now leaves the source line out instead). The grouped specs run against a project declared in groups (`ui/e2e-groups/fixture`), served for real, instead of stubbing `/groups` and `/state`; they also caught the grouped page writing a URL change (a search typed right after opening a group, or after going up a level) against the render's copy of the URL, which reopened the group: changes are now applied to the URL as it is.
- Stop metadata backups from holding up every other barca process: a `barca serve` with a store hashed and copied the whole database under its lock, so a CLI command beside it waited for the copy (about 6 s per command against a 1.5 GB database, now 0.02 s). Only the log fold takes the lock; a read transaction keeps the file still while it is copied.
- `scripts/check-fast.sh` in a tree without a `.venv` (or whose `maturin develop` failed) skips the Python tests with one line saying how to make the venv and exits 1, instead of running hundreds of tests against a missing binary.

## 0.39.5 (2026-11-04)

### Bug Fixes

- UI: one state vocabulary everywhere. A task or sensor shows its last execution (Healthy, Failed, Not run) in tables, as group health already did; Not run, Unknown and Always runs are grey wherever they appear, leaving the queued colour to queued run work; "State unknown" is "Unknown". Node, file and group names use the sans-serif instead of monospace, dark-theme borders and hover/selection backgrounds have more contrast, and table rows stack a name over its file at any width, with the table beside the node panel leaving out last success, typical and next run instead of squeezing every column.
- One `CacheState` enum (`crates/barca-core/src/cache_state.rs`) is the only spelling of a node's cache state: `barca status --json` (`cache.state`, `summary`), the `--state` filter, the server's node state and the UI's generated types all use its eight values (`cached`, `unsynced`, `stale`, `never_run`, `partial`, `blocked`, `unknown`, `always_runs`); a state from a newer server decodes as `unknown` instead of failing the client (#802). `specs/behavior/cache-decisions.md` records it.
- A `--read-only` server reading beside a writer no longer loses reads to `database disk image is malformed` (a cache lookup counted as a miss, a failed request). Its check of the database file's header closed a descriptor of the file, which drops every SQLite lock the process holds on it, so a writer in another process closing its last connection took the database as its alone: it checkpointed past the reader's snapshot and deleted the write-ahead log under the reader's queries. barca now reads the database file only through a descriptor it keeps open, as SQLite does. The same applied to a `barca serve` process's own reads during its metadata backups.

### Performance

- A run that executes a step writes its cost estimates in the ledger's transaction instead of a second one: one commit (a sync of the metadata log) fewer, about 0.7 ms of the one-asset pipeline's 38 ms on the benchmark machine (`benchmarks/RESULTS.md`, 2026-11-04).

### Documentation

- Measured 0.39.4 on the trivial pipeline: 31.7 ms from process start to exit (`benchmarks/trivial/bench.sh`: 42-43 ms, with its shell wrapper), against 26.3 ms for 0.1.5 and 60.3 ms for 0.28.0 on the same machine. The README and the framework comparison now give this number instead of 0.28's; `benchmarks/RESULTS.md` attributes the time per phase, and the manual gives the measured cost of sorting a streamed partition (about twice the unsorted write).
- CI's merge tier runs the performance budgets (`scripts/perf-gate.py`: the trivial pipeline's wall time, coordinator CPU and memory, and the `incremental_dag` coordinator budgets), so a regression fails the merge queue (`specs/behavior/performance.md`, "Gates").
- The deprecated HTTP aliases (`POST /run`, `/run/{target}`, `/get/{target}`, `DELETE /run/{run_id}`, `GET /status/{run_id}`, `GET /events/{run_id}`) and `barca.Client` have one stated lifetime everywhere, the manual, the CLI contract, the OpenAPI spec and `Client`'s `DeprecationWarning`: no scheduled removal; a removal migrates `Client` first and lands as a breaking change in a minor release.
- CLI contract: `partitions.failed` and `batch_execution` are marked new in 0.38.0 instead of "the next release"; a stale note promising a change to the `run failed:` line (shipped in 0.24.0) is gone; `steps_page.nodes[].status` and `blocked_by` (from `history RUN_ID` with a server) are listed as experimental, and `failing_since`, `consecutive_failures`, `steps_failed` and `first_failure` as HTTP-only.
- `BARCA_TRANSFER_RETRIES` is documented, and the configuration page now lists `BARCA_LOG`, `BARCA_SLACK_WEBHOOK_URL`, `BARCA_SLACK_NOTIFY`, `BARCA_UI_URL`, `BARCA_TRACE_LIBRARIES` and `BARCA_SERVER_TOKEN`. A test keeps every `BARCA_*` variable in the source documented in the CLI contract or named as internal.
- The landing page, Getting Started, Philosophy and Architecture no longer say barca never imports your source: it reads source statically, and a nonliteral `partitions(...)` or `batches(...)` expression and a `batches_by` function are evaluated in a resolver subprocess that imports the module.
- The cache reason `not_planned` (state `unknown`), which `status` could already report, is listed in the OpenAPI `Cache.reason` enum, the CLI contract and `barca docs status`; the contract's lists also gain the `blocked` state and the `upstream_failed` reason. Tests keep the code's states and reasons equal to the OpenAPI enums.
- The OpenAPI spec documents a skipped step's reasons, including `upstream_key_failed`, and `specs/README.md` states each owner's status for the current code (token auth and event replay shipped in 0.36.0).
- DIRECTION.md carries the 0.39 cleanup's order (#800), the releases 0.36.0 to 0.39.4, and the November decisions: the skipped cleanup weeks, shared in-flight steps kept on as experimental with the non-goal amended, the UI as a view of the ledger in the CLI's state words, and store inspection settled by 0.36.0.
- Sharing in-flight asset steps between a server's runs (0.39.4) is labelled experimental in `barca docs scheduling`, `barca docs server-mode`, the `serve` entry of `barca docs contract` and the specs, with its three known gaps (#654).
- `barca docs contract` lists which experimental commands graduate in 0.40.0 (`sql`, `history RUN_ID`, `doctor`, `state reset`) and which stay experimental and why, and moves `get -o json|pretty` and `run -o/--output` from the experimental items to a new "Deprecated compat flags" section: they keep working, and none is removed before at least one minor release after a notice naming the replacement.
- `barca docs partitions` ("When a consumer's keys are stale") and `barca docs cache` say what a refresh does not reach: a refresh recomputes its own target's cone, so after a refresh with another target that wrote new bytes, a consumer outside that cone stays cached on its old result and `barca status` reports it `cached`. `--batch` (`batch` over HTTP) groups keys and recomputes nothing on its own; name the consumer in `--refresh`. A test pins the refresh cascade to `partitions_from` consumers, batched and unbatched, on the CLI and under `barca serve` with an artifact store (#799, which is not reproduced).

## 0.39.4 (2026-11-03)

### Features

- Compute an asset step once when two runs of a `barca serve` need it at the same time: the second run waits for the first and reports the step `cached`. Tasks, sensors and differing run hashes stay separate (#654).

### Bug Fixes

- Record the steps a server run requeued by a deploy re-executes on the new code under their new run hashes, so the resumed run no longer serves and keeps the earlier attempt's results (#794).

## 0.39.3 (2026-11-02)

### Bug Fixes

- Automatically split failed partition batches into smaller calls, including after worker crashes, down to single partitions.

## 0.39.2 (2026-11-02)

### Features

- Add `barca.sql()` to the Python API for querying cached results with SQL.

### Performance

- Scope UI duration estimates to each node’s partition history, avoiding scans of unrelated materializations.

## 0.39.1 (2026-10-31)

### Features

- Refresh a key or set with `--refresh 'source[account=A],source[account=C]'` while retaining the complete target DAG, cached siblings, batching and full fan-in publication (#784).

### Bug Fixes

- A metadata database past its share of `[cache] max_size` no longer evicts artifacts that fit. Upgrading a 40 GB database could delete every current result during the trim, costing a recompute of each.
- `barca serve` no longer panics now and then as it exits after a drain. The exit code was already 0.
- A cache lookup the database cannot answer is a miss with a warning instead of a panic. A `--read-only` server beside a writer running `VACUUM` was seen to read `database disk image is malformed`; the database itself was intact.

### Documentation

- `contributing/testing.md` describes `tests/simulation/sim.py`, a seeded simulation of long server use with faults.

## 0.39.0 (2026-10-31)

### Features

- A failed partition key blocks only the keys that read it. A `partitions_from` consumer (or a
  node on the same dimensions) runs its other keys and skips its own key of the failure as a
  step with reason `upstream_key_failed` naming the failed key; a `collect()` fan-in, an
  unpartitioned node or a node on other dimensions is still skipped whole (`upstream_failed`).
  `barca status` and `GET /state` report such a consumer `blocked` (detail `39 of 39 missing
  keys wait on 'demand_forecast'`), `--state blocked` selects them, `summary.blocked` counts
  them, and the UI labels them `Blocked · N keys wait on upstream`. (#764)
- The run page lists every planned node with what became of it: ran (keys, failed keys,
  batched calls, compute time labelled as summed over the calls), cached, or skipped with the
  failed node that blocked it, and states the target's outcome at the top (`publish_plan did
  not run: upstream demand_forecast failed 7 keys`). Successful keys sit behind their count,
  failed keys open at once. The runs table's Steps column counts keys in one unit (`3,077 ran ·
  3 cached`) and a batched node's failure reads `demand_forecast: 7 keys failed · …`.
  `GET /runs/{id}` gains `steps_page.nodes[].status` and `blocked_by`, and `GET /runs`
  `steps_failed` (experimental). (#758)
- The node panel of a partitioned node is actionable: the key bar always shows its failed
  segment and the legend says `7 failed` in red; typical time is per run in the table
  (`~2.2s / run`) and per batched call in the panel (`0.4s / call · 5 calls`), never
  `0.0s / key`; last attempt and history come before the key list, which starts closed past 10
  failed keys; error lines are in the failure colour, wrap, and have a copy button; a key
  search that matches no failed key widens to every key and says so; each key row offers **Run
  this key** and shows its artifact path; the source path is project-relative with **Copy
  path**, and the editor link appears only for a root listed in localStorage
  `barca.editor-roots`. (#763)
- The command palette (⌘K) finds a partition key: a query that looks like one (`ra-02541`,
  `account=ra-02541`) asks every partitioned node and lists `demand_forecast ·
  account=ra-02541 · failed 2m ago` rows that open the node panel with that key searched; with
  an empty query, nodes and groups come before runs. (#759)
- The UI shows every absolute time in one zone, local or UTC, chosen with a toggle next to the
  theme switch and remembered like the theme. Every absolute time names its zone, relative
  times keep the absolute time in their hover title, and the runs histogram, log timestamps,
  next-run cells and schedule times all follow the setting. (#761)
- The graph is readable at fit-to-screen: a fit never scales cards below 1:1 (13px names, 12px
  state labels); a bigger graph is centred on the selected node, else on its failed nodes. A
  consumer that did not run says on its card which failure upstream blocked it. Arrow keys move
  the selection, Enter opens the panel, Escape closes it. (#767)
- Starting a run from any page (a node's Run button, the Failures view's Retry, the topbar)
  shows one line with the run id link, its progress, and its outcome (`succeeded in 2.3s`,
  `7 keys failed`), refreshing the rows it touched. A group's Contents reads `3 assets · 1
  task`; a node's Duration is its last run's time summed over its keys and batched calls, with
  the breakdown on hover. `GET /state` rows gain an optional `last_run`. (#766)
- The UI's tables are compact: 44px rows whose headers never wrap, and beside the node panel a
  table scrolls sideways instead of growing. A failed row's error is 13px in the failed colour,
  sub-labels are 12px, identifiers (node, file, key, run id, target, error) are in the mono
  font everywhere, and a name that does not fit is cut at its start but stays one piece of
  text, so selecting a row copies `demand_forecast`, not `deman\nd_forecast`. The grouped
  view's header fits a 1280×720 window on one line, and the connection dot is 12px with its
  text on hover or focus. (#765)

### Bug Fixes

- The Failures view heads a partitioned node whose keys failed with their most common key error
  (`BatchKeyFailed: … for <key>: …`, `7 of 3,077 keys` under it, `+N more errors` when they
  differ) instead of "failed, with no error recorded"; its cause headings show a real message
  instead of `#`-masked text; "Copy error" copies the key's error and "Open run" opens the run
  that recorded it. (#757)
- A page no longer stays on "barca serve is not reachable" after the server restarts: every
  query asks again every 5 s while its last fetch failed and refetches as soon as a health poll
  succeeds. While the server does not answer, a banner on every page says so, the connection
  badge is an amber pill, and every page in error has a Retry. Schedules no longer shows
  counts next to a "can't load" error and keeps the last loaded rows under a notice when a
  refresh fails. (#760)
- A `group(...)` call that is not assigned to a variable is registered (as
  `group:<file>:<name>`) instead of being silently dropped from `barca list --groups`,
  `GET /groups` and the Assets page. (#762)
- The Tasks page's default view also shows tasks that ran in the last 24 hours, its "Last run"
  counts include them, and zero-count state chips are hidden. (#766)

## 0.38.0 (2026-10-31)

### Features

- A batched run says how many function calls it made. Each batched node gets one line before the
  end-of-run line, also kept in `barca logs`: `[barca] batch:p.py:orders 7 calls, 3077 keys |
  import 0.1s, load 1.2s, compute 8.4s, split 3.1s, write 14.2s`, then `[barca] metadata 2.1s
  recording this run's results`. `get`/`run` JSON gains `steps[].batch_execution` and
  `metadata_seconds` (experimental). The per-key `step:` lines are unchanged.
- The UI works with partitions at scale. A node with failed keys reads `40 of 3,077 keys failed`
  instead of stale, and its panel shows cached, failed and stale keys, the errors grouped, and a
  searchable list of keys. A run lists its nodes with keys, failures and calls, failed first, in
  place of a flat list of keys. `GET /assets/{name}/partitions` is new; `barca status --json` and
  `GET /state` gain `partitions.failed` and `failed_keys`, and `barca history RUN_ID --json` gains
  `steps_page.nodes` (experimental).

### Bug Fixes

- `barca stats`, and the UI's history of a node, reported no runs for a partitioned node. It now
  counts runs, with time summed over the run's keys and `keys` and `failed_keys` on each entry.

## 0.37.1 (2026-10-30)

### Bug Fixes

- The metadata database no longer outgrows the size limit and pushes artifacts out. A production
  database had reached 41 GB against a 13 GB limit whose artifacts took 7 GB, 36 GB of it the same
  change digests repeated on every partition key's row. A definition's digests are now stored once
  (`definition_components`), and each database keeps to its own share of `[cache] max_size`, a
  tenth up to 1 GiB: past it, barca clears digests of old results and stored outcomes of old runs,
  then `VACUUM`s, and only if that is not enough deletes old results whose file is gone and all but
  the last 1000 runs, with their logs. On a copy of that database this took 1m44s and left 848 MB
  with every result and run kept. Digests recorded before this release are cleared the first time
  this runs, so those nodes say a plain `changed` until they next run (`barca docs cache`, "The
  metadata database").

## 0.37.0 (2026-10-30)

### Features

- The UI has a command palette (`⌘K`) for any node, group, file or run, plus Run and Open graph.
  The node panel says what changed (`Failed after a code change`, `Same code, new input`), links
  the definition (`billing.py:142`, opens in VS Code), copies the artifact path and shows the
  first rows. Runs filters and the graph direction live in the URL (`?status=`, `?q=`, `?hour=`,
  `?dir=`). The Run button counts assets and sensors, which is what a get runs.
- `barca list --json` and `GET /assets` gain `file` and `line`; `cache.change_causes` in `barca
  status --json` and `GET /state` splits a stale or failed node's change into `code_changed`,
  `inputs_changed`, `env_changed` and `upstream_stale`, and `cache.changed` is now also set on a
  failed node that once succeeded. `GET /assets/{name}/schema` takes `?sample=N` (at most 20).
- The UI's graph answers a question: All pipelines is a map of files (one weighted edge per pair
  of dependent files, drawn for the selected card when there are too many to read), a pipeline
  of one group opens that group, unconnected members lie in a grid, inputs from outside a group
  are faded ports, cards drop the `ASSET` label, and the legend clears the zoom controls.
- The UI spends its space on the table: title, breadcrumb and health share one row so a table
  starts within 120px of the top; run rows are one line; the queue appears only when something
  runs, waits or is held back; the server version moves to the tooltip of a connection dot in the
  top bar; List/Graph sits in the view it applies to. The sidebar collapses and lists each file's
  groups; long node names are cut in the middle. The node panel leads with the error, with one
  compact stat row. Tasks, Failures, Runs and the node panel show errors the same way (headline,
  traceback on expand), and faint text now meets 4.5:1 contrast. The grouped page's "Needs
  attention" filter is gone: Failures covers it.
- The UI's Failures view groups failures by cause (first line, numbers normalised) with their
  count and impact, biggest first, and each row can retry, copy the error, open its run, or be
  muted in this browser until the node recovers. `Waiting on it` shows `0` for a leaf and `?`
  while the graph loads. Grouped tables open worst first with a `?sort=` control, show Failures,
  Age and Duration instead of Output, and name rows by the distinct part of their id. Tasks open
  on what needs attention; **Show all** (`?state=all`) lists the rest.
- The UI uses one name and one meaning per state. Green means healthy only: links, selection and
  the primary button are neutral; red colours only the failed count, a column of states is dot and
  text (pills stay for failed and running). States read Healthy, Stale (`Stale (some keys)`),
  Failed, Running, Not run in every badge, legend, card and tooltip, and a check is `check`
  everywhere (a task named `check__*` or `validate__*`). `get` is gone from the buttons: **Run**
  names its target and size (`Run invoices_00`, `Run billing.py (506 nodes)`), works with nothing
  selected, and `Run all 8,070 nodes` asks first; the count is the assets and sensors a `get` runs.
- `GET /runs` rows have `first_failure` (`node_id`, `error`), and `GET /state` rows have
  `last_success_at`, `failing_since` and `consecutive_failures`. The UI shows them: asset, group and
  failure tables say how late each row is (Last success, failing since, runs failed in a row); a
  failed run with no run-level error names its first failed step and that error; Steps reads
  `21,950 / 28,404 · stopped at <node>`. Every time is relative with absolute UTC on hover, in one
  format. The Runs activity strip has an hour axis, a now marker, a tooltip per bar naming what
  failed first, and clicking a bar filters the list to that hour.

### Bug Fixes

- The UI's minimap draws its nodes (it was empty), appears only from 60 nodes and fits the graph
  beside it; names just over 10 characters also cut in the middle in narrow table columns.
- UI: Reset database shows only for a database barca cannot use, not on every page. Sidebar health
  dots use whole-project state and show no dot until a file's state is all known, instead of
  judging a file by the part in view. All pipelines → Graph fits every domain on screen with nothing selected.
  `#/?pipeline=…&show=failures` keeps its filters through the redirect to Assets.

## 0.36.1 (2026-10-30)

### Features

- The UI answers "what is broken, and why" on projects of thousands of nodes. Every view shows
  its scope's health as a bar (`77 failed · 2,111 stale · 7,912 healthy`), group rows show the
  same breakdown, and a Failures view lists every failure in scope with its error and how many
  nodes wait on it, most impact first. A node's panel leads with its error, traceback and run,
  or with the failed upstream nodes a stale node waits on, and counts what depends on it. Search
  reaches every node in scope, rows are one line, and a graph above 1,500 nodes draws the
  selected node's lineage instead of never finishing its layout; graphs open at a readable zoom,
  on the selected node, rather than shrunk to fit. The Runs page shows the last
  24 hours hour by hour with filters for runs that did not succeed, and each failed run's error.

- Experimental `GET /state` takes group ids in `targets` (`group:FILE:NAME`, every node in the
  group) and `files=` (every node of a source file). The UI scopes a group or pipeline that way:
  it listed every node id in the URL, which a server refused (`431`) for a pipeline or group of
  a few hundred nodes, leaving its states unavailable.

## 0.36.0 (2026-10-30)

### Breaking

- Server event IDs use an attempt epoch and one-based sequence. Replay is bounded to
  65,536 events or 16 MiB per run, 64 MiB across windows; expired replay returns 410,
  another attempt returns 409, and malformed/old numeric cursors return 400. CLI and browser
  clients resume acknowledged events; expired replay warns and follows the canonical outcome.
  `logs -f --server` exits 3 on an unfillable transcript gap. Older servers keep replay-count fallback.

- Served runs' end-of-run metadata backups now share the routine backup interval. Completion
  still records the run locally, but no longer promises an immediate remote metadata copy.
  Standalone final backups and the server's graceful-shutdown backup remain enabled.

- Status inspection can reuse cloud availability and remote shape observations for five
  minutes. Local file changes invalidate evidence immediately; required reads and ledger
  publication still verify directly. This bounds repeated inspection work without changing
  asset freshness rules.

### Performance

- Reuse bounded, file-identity-keyed digest and shape evidence across status requests and
  metadata refreshes; prefer verified local shapes. Keep at most 8 MiB/8192 entries per
  environment, limit individual values to 64 KiB, and bound cold inspection readers to eight.
  Refreshes, confirmed upload repairs and credential/import changes invalidate affected evidence.

### Features

- Resume CLI and browser run streams using attempt-scoped `Last-Event-ID` cursors. Replay
  retains at most 65,536 events/16 MiB per run and 64 MiB across runs. When replay has expired,
  execution clients show a gap notice and follow the accepted run's outcome without
  resubmitting work; `logs -f` reports incomplete output as an infrastructure error.

- Connect the UI with a bootstrap code or API token, manage named tokens, and show new
  secrets once with explicit expiry choices. Tab-scoped credentials cover ordinary requests
  and fetch-based event streams; disconnect/revocation clears the connection and UI cache.

- Opt into server-issued project bearer tokens with `[server] auth = "token"`,
  `BARCA_SERVER_AUTH`, or `serve --auth`. Local `tokens bootstrap` creates a one-use recovery
  grant; HTTP token creation/list/revocation works in read-only mode, stores digests outside
  metadata backups, and closes revoked or expired event streams.

- Send server bearer credentials from `BARCA_SERVER_TOKEN`, or Basic credentials from URL
  userinfo, consistently across execution, ledger, streaming and cancellation requests in the
  CLI and deprecated Python client. Reject mixed credentials and redirects outside the
  configured server origin/path, redact reflected credentials in HTTP errors, and keep the
  reserved bearer variable out of Python helper environments.

- Report local metadata backups outside the cache budget while retaining the five-copy
  rotation. Doctor and GC distinguish budgeted, backup and physical bytes; status adds an
  experimental project-wide `disk_usage` summary using maintained cache accounting without
  walking artifacts. Physical totals continue to include backups.

- Configure routine metadata backup cadence with `[remote] backup_interval` or
  `BARCA_BACKUP_INTERVAL` (default `"1m"`, or `"off"`). A shared per-database gate applies
  before copying, hashing and uploading, survives process restarts, and retains dirty
  recorder progress when a request is skipped.

### Bug Fixes

- Avoid retaining inspection evidence for files changed within the last second, preventing
  rapid delete/recreate identity collisions from preserving stale local values.

- Detect release merge groups from their checked-out commit so they run full emulator,
  DuckDB and integration gates even when the CI event context omits commit-message data.

- Restore a metadata file onto a fresh host by creating its destination metadata directory.
  Restoring ledger backups leaves project authentication state separate and unchanged.
- Validate token creation expiry while holding the auth-state lock, using the same private
  clock as bootstrap and validation; deterministic clock tests cover expiry boundaries and rotation.

### Documentation

- Record RFC-0016's accepted design for opt-in server tokens and attempt-scoped event-stream resume, including
  bootstrap, browser credential handling, revocation and bounded replay. The implementation adds bounded replay with CLI and browser recovery.

- CI runs in three tiers: a pull request runs the static checks, a merge group runs those plus
  the Rust tests, the wheel and the quick Python subset in `scripts/fast-tests.txt` (about two
  minutes), and a release PR and its merge group run everything, with the object-store
  emulators, the duckdb 1.x rerun and the minute checkpoint test on. `scripts/check-fast.sh`
  builds the venv's barca with the `fast` profile instead of `--release`, and runs the same
  quick subset. The release workflow builds the UI inside each wheel job from a cache keyed
  on `ui/` instead of waiting on a separate job, waits for the published wheel on PyPI once
  instead of in each verification job, and skips the barca.sh deploy with a warning when the
  Cloudflare secrets are missing instead of failing the run.

## 0.35.3 (2026-10-29)

### Bug Fixes

- A batched step's DuckDB, Arrow or polars table is split into keys sorted by every column, so
  a refresh that returns the same rows in another order keeps its `cutoff=True` consumers
  cached; before, a parallel producer's row order changed the artifact bytes and reran them.
  The first refresh after upgrading rewrites each key's artifact and reruns its consumers once.
  Values that compare equal but differ in representation (`-0.0` and `0.0`, distinct NaNs,
  strings under a case-insensitive collation) still need normalizing in the step.

### Documentation

- Record the accepted configurable metadata backup cadence and separate local-backup
  accounting design in RFC-0015 for #706. Runtime behavior is unchanged.

## 0.35.2 (2026-10-29)

### Features

- Make CLI help concise, group root commands by purpose, and add `--examples` on commands
  and nested commands for the complete tested example collection. Detailed behavior stays
  in the built-in manual; help and examples run without loading a project.

- Experimental `GET /state?targets=a,b` computes only those nodes and their upstream
  dependencies, including duration queries; omitted or empty targets keep the whole graph.
  This lets the UI request status for the open pipeline (#708, #710).

### Bug Fixes

- The release helper checks its changed-file allowlist in a fixed locale, so a valid
  version bump also passes on hosts whose filename collation differs from byte order.

- Server URL userinfo is omitted from CLI errors and announcements, ledger warnings, stream
  drop messages, SQL provenance and doctor output (including JSON and older recorded errors).
  Requests retain their URL credentials, and pending posts keep matching the same server
  (#706, credential masking).

- The grouped assets page now runs the selected node from the topbar in both list and graph
  views, and every node panel has the same action: cache-aware `get` for assets and sensors,
  `run` for tasks. Read-only servers disable both controls. Accepted runs show as running
  before the first stream event; their details are available through View run (#709).

- The assets page and sidebar render groups, graph layout and pipeline counts from metadata
  before status arrives. Cache state shows as checking and fills in without moving rows or
  graph nodes; a status error leaves the graph visible. Opening a pipeline or group requests
  only its upstream status cone, while All pipelines requests the whole graph (#710).

### Performance

- `barca serve --read-only` reads current WAL metadata in place, warms status at startup even
  with an external writer, and moves fallback copies, migrations and duration reads off the
  request threads. Latest sensor observations use indexed seeks per key; eviction checks
  group the eviction index before checking for a later restore. On the synthetic #708 fixture
  (`benchmarks/planning_pipeline/bench_status.py`: 27 files, 373 nodes, seven groups, 500,000
  observations, 1.15 GB DB), `/state` takes 0.42 s with a writer committing every 0.2 s;
  concurrent `/health`, `/assets` and `/groups` peak at 0.009 s (19 samples per route).
  This measures inspection of synthetic history, not the production deployment (#708).

### Documentation

- Simplify first-use documentation around cached assets, tasks, partitions and remote
  execution. Start with the base install; make SQL and shared-store setup optional, and
  distinguish server execution from local computation against shared server state.

- Bring DIRECTION.md through 0.35.1 and record the decided order after it: the
  0.35.2 server/UI fixes, 0.36.0 backup/credential/resume RFCs, then feedback cleanup.
  Keep unresolved partition costs open and distinguish approved direction from shipped behavior.

- Record reproducible overlap, bootstrap and inspection measurements for #654–656,
  including a synthetic 1,000-node/1.2-million-key workload. These gaps remain open;
  the benchmark records distinguish coordinator memory, worker costs and responsive
  health checks from expensive status recomputation.

## 0.35.1 (2026-10-28)

### Bug Fixes

- `barca serve` no longer stops starting runs while it measures `.barca/` against its size
  limit. A full walk (at start-up without `.barca/gc-state.json`, at least once a day, when
  over the limit) read every metadata row in one query under barca's lock and looked at the
  file of every row on one of the server's two event-loop threads: on a project with 465,000
  artifact files and 2.8M results, a run asked for at start-up was accepted after 209 s and
  took 232 s. The walk now reads 20,000 rows at a time under the lock, runs on a thread of its
  own, takes file sizes from the walk instead of looking at each row's file again, and a run
  that ends while it walks skips its own walk: the same run was accepted at once and took
  33 s. A full walk of a project with its artifact files in place (465,000 files, 2.8M
  results) takes 148 to 159 s instead of 280 to 290 s, and plans the same evictions.
- Runs with a remote artifact store no longer fail when planning takes longer than 10 s: the
  transfer helper's hello is answered as soon as it connects, instead of after planning finishes
  (#702).

## 0.35.0 (2026-10-28)

### Bug Fixes

- `barca serve` loads a `partitions_from` consumer in another file than its source when the
  source's keys come from an expression (`partitions(KEYS)`). The server evaluated the
  expression again for each consumer, in the consumer's module, and refused the node with
  `NameError: name 'KEYS' is not defined`; the CLI loaded it. Each expression is now evaluated
  once per module and load, so the server also starts in 0.3 s where a project of 1,000
  partitioned nodes took 19 s (960 interpreters started, now 10).

### Performance

- Deciding the cache state of many partition keys is faster again: `barca status` over 1,000
  nodes with 1.2M keys takes 7.5 s instead of 12.2 s, and so does `GET /state` and the
  decisions every run makes. Since 0.34.0 a one-key cache lookup compiled its SQL again for
  every key (the move from `turso` to libSQL lost the statement cache); a reader now compiles
  it once, and the keys of a partitioned step are looked up a few thousand to a query.
- `GET /runs` and `barca history` read each listed run's first failed step from a new partial
  index, `idx_mat_failed`, instead of every row of the run: 115k rows for a run of a 115k-key
  node, read again on each poll of the web UI's runs list. Opening an older metadata DB
  builds the index once, about ten seconds for 3M rows.
- The web UI stays responsive with a thousand nodes. The assets and groups tables render the
  rows in view only (26 instead of 1,000 on a 1,000-node project; with the CPU slowed four
  times, a poll during a run no longer blocks the page, where it held it for 1.8 s), and
  keyboard navigation and sorting work as before. A graph of 300 nodes or more is laid out
  off the main thread, and the minimap is left out above 400 nodes, where it was most of the
  cost of a pan.
- `barca serve`'s read routes no longer stall a run, or wait for one. They read the metadata DB
  in place, beside the run writing it (SQLite's WAL mode), instead of taking the lock the run
  needs and copying the whole database first (gigabytes for a long history, written in full
  on Linux). A `--read-only` server still reads copies. `GET /state` after a change waits at
  most a second for the new cache states and answers the last ones meanwhile (what is running
  is always current), and a failure to compute them is answered. On 1,000 partitioned nodes
  with 2.8M result rows (`benchmarks/wide_partitions`), the slowest read during a 115k-step
  server run polled as the web UI polls took 2.4 s instead of 53 s, and the run 58.5 s
  instead of 88.8 s (on a busy machine; 23 to 27 s on a quiet one).

## 0.34.0 (2026-10-27)

### Breaking

- A decorator value barca cannot read from the source is an error when the file is read (exit
  2, naming the argument, the file and line, and the form to write) instead of being read as
  "not written" (#665): the path of `@sink` (a path that was not a string literal declared no
  sink, so nothing was written), `name=`, `description=`, `inputs=` (not a dict literal, a key that is
  not a string literal, a value that names no step), `partitions=` (not a dict literal, a key
  that is not a string literal, a value that is not `partitions(...)` / `partitions_from(...)`),
  `serializer=` (also on `@sink`; not a string literal, or an unknown format), `freshness=`
  (not `Always`, `Manual` or `Schedule(...)`), `tags=` (not a dict of string literals),
  `timeout_seconds=`, `retries=` and `retry_backoff=` (not a number literal; these used to
  fall back to 300, 1 and 0.0). Replacement: write the value as a literal in the decorator
  (`barca docs assets`, "Values barca reads"); `None` still reads as not written. A
  `partitions([...])` list with an element that is not a string or integer literal is now
  evaluated by the resolver like any other key expression; that element used to be dropped.

### Features

- `barca[sql]` and `barca[test]` accept duckdb 2 (`duckdb>=1.4,<3`, #567); CI runs the
  DuckDB tests on 1.x and 2.x. The floor rises from 1.1 to 1.4: duckdb 1.1 to 1.3 already did
  not work (dataset sinks pass `to_parquet` arguments 1.1 rejects), and on 1.4 a remote CSV
  input read as a DuckDB relation failed with `Connection has already been closed`; it is kept
  open now. Barca no longer calls relation methods duckdb 2 removed or deprecated (`barca sql
  --limit` failed with `fetchone()`), and discards a result a step left unread on the shared
  connection, which duckdb 2 otherwise lets block writing the step's relation and the next
  step on that worker. Pipelines may need changes for duckdb 2
  (`rel.fetchone()`, unread `execute()` results, `->` lambdas), and a database file duckdb 2
  writes cannot be opened by 1.x: `barca docs duckdb`.

### Bug Fixes

- Preserve current partition results when a cache hit reuses an older materialization. Bound
  historical selection evidence per logical node and fall back to full validation on overflow.

- A `collect()` fan-in annotated `list[duckdb.DuckDBPyRelation]` no longer fails with duckdb 2
  ("connection is busy with another call"): the worker made the relations on its one shared
  connection from several threads at once. It now makes them one at a time (#663).

- A failure in barca's own code around a step (reading its inputs, splitting a batched call's
  result, writing its result or its sinks) keeps its full traceback instead of reporting
  `"traceback": null`, and the error says it failed in barca. Step errors carry a new `phase`
  (`load_inputs`, `user_code`, `split_output`, `write_output`, `sink`; `null` when unknown) in
  the error envelope, `barca.BarcaError.phase`, and as `error_phase` in `barca history RUN_ID`
  and `GET /runs/{run_id}` steps (#664).

- A sink is republished when its asset is served from cache under another run hash (#662).
  Barca records the run hash each `@sink` last published (per partition key for partitioned
  and dataset sinks, in a new `sink_publications` table of metadata format 1); a cache hit
  whose run hash differs writes the sink from the cached artifact without running the step,
  and one that matches writes nothing. Code A, then B, then A left the sink holding B.

### Refactoring

- Make freshness-proof replacement an explicit guarded transaction and cover interference,
  cache-only deletion and rollback; preserve the existing caller-held database locks.

- Give durable result history one internal owner across progress, terminal, legacy-cache and
  server-ledger writes, preserving their transaction, receipt and deduplication rules.

## 0.33.1 (2026-10-27)

### Refactoring

- Separate collected run results, attempt/timing evidence and ledger/report projections from
  execution decisions, preserving persistence, upload confirmation and cancellation ordering.

### Documentation

- Consolidate freshness, execution, inspection and performance contracts; separate approved
  design from implemented behavior and unscheduled ideas. Remove superseded plans and
  check specification navigation, invariant ownership and finite concurrency models in CI.
  Record approved first-use, terminal feedback, project resource and agent interaction bounds
  without prescribing implementation; include a synthetic navigation scenario. Capture
  freshness requests, retries, snapshots, drain/recovery and configurable artifact retention,
  with remaining requirements questions saved for resuming the discussion. Map current
  architecture ownership and stage refactors with explicit implementation acceptance gates.
  Separate approved behavioral specs, interface contracts, unapproved proposals and engineering
  notes; standardize behavioral sections, remove completed plans and enforce the structure.
  Record a pre-merge audit with separate unresolved decisions, implementation disagreements
  and specification defects, plus controlled baseline reproduction probes. Correct audit
  blockers in the declaration model, upload/digest/log contracts and multiline link checking;
  separate invocation retry intent from the unresolved physical retry policy.

## 0.33.0 (2026-10-25)

### Breaking

- Sensors now honor explicit change signals: `True` advances freshness even without data,
  `False` retains the previous observation, and `(changed, value)` also passes data.
  For content comparison, return the version value alone instead of `(True, value)`.
  Worker protocol 5 requires matching native and Python packages.

### Performance

- Default DAG runs reuse validated freshness per node before expanding partition keys.
  A changed key schedules only its affected chain; fresh keys keep their cache and history
  counts without coordinator objects for every key. Target selection also plans only its
  dependency cone. Workers compare source snapshots and report compact observations;
  scheduling priors and routine cache accounting avoid scanning every partition.
  The native coordinator uses the bundled, local-only libSQL driver and one CLI event loop.
  Existing SQLite-format history remains readable. Sensors still discover their keys,
  and changed fan-ins read all inputs. Explicit partition selections stream only requested
  source values into Rust, including cold runs and dry-run predictions.


## 0.32.0 (2026-10-24)

### Breaking

- Worker and transfer helpers now negotiate protocol 4. Protocol 3 helpers cannot execute
  a whole batch in one call and are refused before work starts, with an error asking to
  reinstall matching barca packages. The standalone stderr protocol stays at version 2.

- `barca run` and `barca.run()` on a partitioned task now return every key's value in a
  dict keyed by partition key, with tables as JSON rows, on the server too. Previously they
  returned only the first key's value. This also applies to a single `--partition` selection:
  read its value from the keyed dict. More than 1000 keys use the same bounded summary as
  `get`; select keys explicitly to read them all. Unpartitioned task output is unchanged (#643).

### Features

- Batched partitions (experimental, RFC-0013, #643): `batch=N`, `batch="all"`, `batches([...])`
  and `batches_by(fn, max_size=N)` on partitioned assets and tasks group stale keys into
  fewer function calls. Each key keeps its own run hash, artifact, cutoff and sinks.
  A batch function receives lists of keys and keyed inputs, and returns a keyed dict or
  a table split by its partition columns; `barca.Failed("reason")` fails one key alone.
  `--batch` on `get` and `run`, `barca.get(batch=)`, run requests and schedules override
  grouping. History groups the keys of each call, and dry runs describe the grouping.

### Bug Fixes

- Await kill/wait jobs for rejected and disconnected worker starts before shutdown
  returns, preventing a fast CLI failure from leaving its helper alive.

- Concurrent dataset sink writers no longer fail when another writer removes the empty
  shared staging directory while a new key starts writing.

- Reading a large metadata database no longer prints internal `WHOPPER_FIND_FRAME_PROBE`
  diagnostics on stderr. Update the Turso dependency to 0.7.0-pre.19 (#645).

### Documentation

- Retain deprecated HTTP route aliases for the public Python `Client`; remove the expired
  promise to delete them after one minor release. New integrations should use `/runs` (#606).

## 0.31.0 (2026-10-23)

### Breaking

- `barca serve` no longer stops a run after 600 seconds. That limit was fixed and undocumented,
  and it cancelled long partitioned rebuilds; runs now have no time limit by default.
  `barca serve --run-timeout 600` (or `[server] run_timeout_seconds = 600`) restores it (#628).
- A run of `barca serve` that times out is recorded with status `error` and the outcome `run
  timed out after Ns (<setting>)`; up to 0.30 it was recorded as `cancelled`. Automation that
  read `cancelled` as a timeout can match the outcome instead; a run cancelled with `DELETE
  /runs/{id}` is still `cancelled` (#628).
- A `partitions(<expression>)` that raises is a load error: `barca list`, `status`, `get`,
  `plan` and `doctor` exit 2 naming the node with the traceback, and `barca serve` reports it in
  `load_errors` and serves the rest of the project. Up to 0.30 it printed a warning and the node
  had no partition keys. Fix the expression, or make it return `[]` when there are no keys
  (#628).

### Features

- Production feedback from a pipeline under `barca serve` (#627, #628). Why a run failed is one
  field away: `barca history --json` runs gain the flat `failed_node` and `error` (beside
  `failure`), so `--fields run_id,failed_node,error` works, and `barca history RUN_ID` shows the
  last 20 lines a failed step printed (`steps[].output_tail`, without the traceback it already
  shows). Per-step memory: the worker resets the kernel's peak counter when a step starts, so
  `max_rss_bytes` is the step's own peak, not the worker's peak over every step it ran (Linux;
  elsewhere it is recorded only when a step raised the worker's peak), measured up to the end of
  writing the result, so a returned DuckDB relation counts. It is in `barca history RUN_ID`
  (`PEAK_RSS`, `steps[].max_rss_bytes` and `cpu_seconds`), in `barca stats` (`max_rss_bytes` and
  `median_max_rss_bytes`, and each `recent_runs` entry's `max_rss_bytes` and `cpu_seconds`) and
  in `barca.materializations`. Bytes DuckDB spilled are not recorded (DuckDB keeps no total).
  `barca stats` `recent_runs` entries gain `run_id`. A project setting makes `--env` required:
  `require_env = true` in `barca.toml` or `BARCA_REQUIRE_ENV=1`, and a command that touches an
  environment without `--env` or `BARCA_ENV` exits 2 naming the environments there are; without
  it, a command that names none in a project with several environments prints one note on
  stderr saying which it uses. `@asset(deterministic=False)` silences the "not deterministic"
  warning for an asset expected to return new bytes (caching and the run hash are unchanged);
  sensors never had it. `barca plan` accepts `--json` and `--pretty`, which change nothing (the
  plan is always JSON). OpenAPI specification 12: `GET /runs/{run_id}` steps gain `cpu_seconds`,
  `max_rss_bytes` and `output_tail`, `GET /stats/{name}` the memory keys and `run_id`, and `GET
  /history` runs `failed_node` and `error`.

- The worker pool follows the capacity (#628): without `BARCA_POOL_SIZE`, a run starts at most
  the cores or the capacity, whichever is smaller, so `[execution] capacity = 1` starts one
  worker, not one per core. `[execution] pool_size` in barca.toml sets the pool
  (`BARCA_POOL_SIZE` wins over it). `barca plan` adds `pool_source` beside `pool_size`, and
  `barca doctor`'s `capacity` check names the pool size and where it came from.
- A capacity shared by every run of `barca serve` (experimental, #628): `--capacity N` or
  `[server] capacity = N` admits the `weight=` of the steps of all runs against one budget, first
  come, first served across runs with the head reserved, so a light run proceeds beside a heavy
  rebuild while the heavy steps stay serialized, and a heavy step is never starved by light ones.
  `GET /queue` (`barca queue --json`) has `capacity: {total, source, in_use, runs, waiting}`,
  `GET /health` has `queue.capacity`, and the web UI's Runs page shows the weight in use and the
  waiting steps.
- The run timeout of `barca serve` is configurable (#628): `--run-timeout SECONDS`, `[server]
  run_timeout_seconds`, and per schedule `Schedule(cron, timeout_seconds=N)`, which wins for its
  ticks; 0 is no limit. A run that times out is recorded as `error` with `run timed out after Ns
  (<setting>)`.
- Under a supervisor (process 1 of a container, systemd, Kubernetes) `barca serve` prints the
  stop grace period it needs at startup: `[barca] stop grace: ... must allow at least Ns between
  SIGTERM and SIGKILL (drain timeout ...)`. With a drain timeout under 10 seconds the time it
  takes after the drain shrinks with it (half the drain timeout for stopped runs, at least 2 s;
  1 s for connections), so `drain_timeout = 5` needs 9 seconds and fits Docker's default of 10
  (#628).
- A run that was in flight when a supervisor killed the server during its drain (SIGKILL after
  SIGTERM) is put back in the queue by the next server instead of being recorded as interrupted,
  once per run. A server that dies without a stop signal (the OOM killer, a crash) still records
  its runs as interrupted, so a run that kills its server cannot loop (#628).
- `@asset(cutoff=True)` (experimental): what reads the asset folds in the SHA-256 of the bytes
  it wrote instead of its run hash, so when it reruns and writes the same bytes, everything
  below it stays cached. A global step that reruns with unchanged output no longer reruns every
  partition key below it; per key on a partitioned asset; a cutoff dict `partitions_from` source
  gives each key its own value. Its consumers are decided after it runs; until then `--dry-run`
  and `barca status` report them `unknown` with the new reason `cutoff_pending` (also in the
  `Cache` reason enum of OpenAPI specification 12). A `--refresh`
  cascade stops at a cutoff asset. Turning `cutoff` on or off reruns what reads the asset once
  (`barca docs cache`, "Early cutoff") (#628).

### Bug Fixes

- `partitions(<expression>)` imports the asset's module the way a worker does, with the project
  root on `sys.path`, so an expression that reads a project module from a file in a
  subdirectory works (#628). An expression that raises is a load error naming the node, with the
  traceback, in `barca list`, `barca status`, `get`, `GET /health` and `barca doctor`; up to
  0.30 the node silently got no partition keys.

### Documentation

- The workflow pages 01, 02, 03 and 06 were re-run on 0.30.2 and restamped (#606): their
  `--agent` samples show the `[barca] importing` line, page 06's `status` table agrees with its
  text, and page 03 now says a removed partition key leaves the `barca sql` view and that a
  failed key's fix that changes the code runs every key again.

## 0.30.2 (2026-10-23)

### Breaking

- A partitioned target of more than 1000 keys, returned without `--partition`, no longer lists
  every key in `final_output`: it is `{"_barca_partitions": {"keys", "computed", "cached",
  "outputs", "truncated", "hint"}}`, where `outputs` maps only the keys the run computed (at
  most 1000) to their pointers, values or links, and `-o path` prints those paths with a note
  on stderr. 1000 keys or fewer, and every `--partition` selection, are unchanged. To read
  other keys, select them with `--partition` (`partition=` in Python; `barca.get()` raises for
  the summary) or query them with `barca sql` (`barca docs partitions`, "Many keys"; #623).

### Bug Fixes

- `barca serve` no longer grows without bound under a schedule over many partition keys. At
  200k keys each tick wrote about 82 MB of run outcome to the metadata DB and kept about 400 MB
  more memory; with one key changing per tick it records a 2 KB outcome (the DB grows about
  50 KB a tick) and its memory stays flat. A run's outcome lists the keys it computed and counts the cached ones; finished
  runs are kept in memory under a 64 MB budget as well as for an hour, oldest dropped first;
  the server returns the memory a run freed to the system when it ends; `GET /runs/{run_id}`
  and `barca history RUN_ID` list at most 1000 steps (`?limit=N` / `--limit N`, `?all=true` /
  `--all`) with a new `steps_page: {total, truncated}`. Runs recorded before keep their
  outcomes and read as before (#623).

## 0.30.1 (2026-10-23)

### Bug Fixes

- A metadata DB written before 0.29.2 opens again (#629): 0.29.2 to 0.30.0 refused it with exit 3
  ("its `logs` table has no `stream` column") on every command, `barca serve` included,
  because the check that runs before the in-place migration did not know the `logs.stream` and
  `logs.ts` columns that migration adds. Opening it now adds them in place and keeps every row;
  lines recorded before read as stdout with no time. No `barca state reset` is needed. A test
  now opens a database written by a release of every minor from 0.20 with the current build.

## 0.30.0 (2026-10-23)

### Breaking

- A sink that cannot be written now fails its step, for every `@sink`: the step's error is a
  `SinkError` naming the sink, the run exits 1 with a `step_failed` envelope, and the step is not
  recorded, so the next run tries it again. Up to 0.29 a failed sink printed
  `[barca] SINK FAILED: ...` on stderr and the asset succeeded; automation that grepped stderr
  for that line can rely on the exit code instead (#621).

### Features

- Dataset sinks (#621): a `@sink` path ending in `/` writes a Hive-partitioned dataset, one
  directory level per partition dimension of the asset (`scored/retailer=r1/part-0.parquet`),
  that DuckDB and pyarrow read as one table
  (`read_parquet('scored/**/*.parquet', hive_partitioning = true)`). `partition_by=[...]` adds
  levels from columns of the returned table, `rows_per_file=` caps the rows of each parquet file,
  and `serializer="csv"` or `"json"` (JSON Lines) write one file per partition. DuckDB writes a
  returned relation (streaming), polars a polars frame and pyarrow a pandas frame or Arrow table,
  all with the same layout. A cached key writes nothing; a key that runs is staged and swapped in
  for its own directory; a key the asset no longer has is deleted after the run. The sink owns
  its prefix (`_barca_sink.json`) and refuses one holding files barca did not write, and a write
  that makes many directories or small files prints a warning. Works on local paths and on
  `s3://`, `gs://` and `abfs://`. `barca docs sinks`, "Dataset sinks" and "How to choose
  partition columns".

## 0.29.6 (2026-10-23)

### Features

- Keyed partition sources: `partitions_from(source)` where `source` is an unpartitioned asset
  or sensor that returns a dict uses the dict's keys as partition keys, passes each key its own
  value (as the parameter named after the source, or the `inputs=` key that names it), and
  folds only that value into the key's run hash. One sensor query that returns
  `{retailer: version}` for every retailer then decides which keys exist and which changed:
  only a changed or new key runs, through a chain of `partitions_from` assets, on `barca get`
  and on a `barca serve` tick. `--dry-run` and `barca status` say per key what changed
  (`partitions.changed`, experimental, with the new `value ID` entry), and the dry run's table
  names it. With one key changed, `barca get` of a two-asset chain takes 0.32 s at 10k keys
  against 3.3 s with a sensor partitioned by key (which makes one call per key), and a
  `barca serve` tick at 200k keys takes 4.3 to 4.6 s (`benchmarks/keyed_partitions`,
  `benchmarks/RESULTS.md`). `barca docs partitions`, "Updating only what
  changed, across many partitions".

### Performance

- A run over a partitioned node with many keys does less per key: the metadata DB keeps a
  32 MiB page cache (one pass over 200k keys' rows read the same pages from disk several times:
  2.8 s against 0.25 s), a key read within the last hour is not stamped again (a 200k-key run
  wrote 200k rows: 18 s), each returned key's artifact is checked once instead of three times,
  a key's run hash is computed on several threads, and a partitioned step's keys are not copied
  for each worker chunk. With 0.29.4's key lookups, a warm `barca get` of a 200k-key chain with
  one key changed takes 7 s (31 s before both).

### Bug Fixes

- An unpartitioned `@sensor()` or asset returning a dict into `partitions_from` no longer runs
  its consumer once with no arguments (`TypeError: keyed() missing 2 required positional
  arguments`). A source that returns anything other than a list or a dict, a dict key that is
  not a string or an integer, or a value JSON cannot write fails the source step with an error
  naming the key or the expected types; an empty list or dict gives no keys, and a `collect`
  over them an empty list.

## 0.29.5 (2026-10-22)

### Breaking

- `barca serve` no longer refuses or cancels work when it stops. During a drain (SIGTERM or
  SIGINT) `POST /runs` and the older trigger routes answer `202` (`200` for the aliases) and
  queue the run for the next start, instead of `503` with `Retry-After`. At `--drain-timeout`
  the runs in flight are stopped and put back at the head of the queue, to start again from the
  beginning after the restart (exit 0), instead of being recorded `cancelled` (exit 1); a second
  signal still cancels them (exit 1). A requeued run is executed at least once: an interrupted
  step or task runs again. A `--server` client and `barca logs -f --server` wait up to 60 seconds
  (`BARCA_SERVER_RECONNECT`, `0` for the old behaviour) for a server whose stream broke, instead
  of exiting 3 at once. `POST /runs` answers `{"run_id", "status", "position"}` (was
  `{"run_id"}`); `GET /health` gains `started_at` and `queue`.

### Features

- A durable run queue for `barca serve`: every run it accepts (`POST /runs`, `--server`, the UI,
  a scheduled tick) is recorded in the metadata DB before it answers, starts in order at most
  `--max-runs` at once (`[server] max_runs`, default one per core), and survives a restart: the
  next server prints `[barca] resuming N queued runs` and starts them under the same run ids.
  `barca queue [pause|resume] --server` (and `GET /queue`, `POST /queue/pause`,
  `POST /queue/resume`, a control on the UI's Runs page) shows the queue and pauses it, a pause
  that is kept across restarts; `--read-only` refuses it. `barca docs scheduling`, "Restarting
  without losing work", is the operator recipe, with supervisor grace periods.

## 0.29.4 (2026-10-22)

### Performance

- Inspection is fast at 300 assets with a 200k-key partitioned node and 2,000 runs
  (`benchmarks/read_routes`; numbers in `benchmarks/RESULTS.md`): the web UI's state view
  (`GET /state`) 76 s to 2 ms (2.2 s for the first request after a change), `GET /nodes` 72 s
  to 1 ms, `barca status` 76 s to 2.0 s, `barca status --server` timed out (exit 3) to 13 ms,
  `GET /history` 1.4 s to 42 ms, and `barca get` with every asset cached 14.1 s to 3.6 s.
  Queries that filtered node ids with `substr` or `LIKE` scanned the whole table once per node;
  they use the node id index now, with three partial indexes added within format 1
  (`idx_mat_changed`, `idx_mat_attempts`, `idx_mat_evicted`; built on the first open after the
  upgrade, about 4 s for 400k rows). The dry run reads a node with many keys in one pass. The
  server keeps its parsed graph, one copy of the metadata DB and its status results until a
  source file or the database changes, answers an unchanged poll with `304 Not Modified`
  (`ETag` and `If-None-Match` on every inspection route), and computes `/state` again in the
  background while the UI is open.

## 0.29.3 (2026-10-22)

### Breaking

- `--partition` narrows the whole planned cone, not only the target. Every partitioned node with
  a selected dimension computes only the selected keys, upstream and downstream: `barca get
  margin --partition region=apac` no longer brings every key of `sales` up to date, and
  `--refresh sales` with it recomputes only `sales[region=apac]`. A selected key has the same run
  hash and artifact as in a full run, so a later full run serves it from cache. A fan-in that
  `collect`s a narrowed node, and what is below it, is not run: the step reports it `skipped`
  with reason `partition_selection`, stderr names it in one note, and the run still exits 0. The
  old target-only behaviour is gone, with no flag to keep it; run without `--partition` for every
  key. This breaking change ships in a patch release (0.29.3) by choice.

### Features

- Debugging a failed job from the CLI, here or from a server. `barca history` takes
  `--status failed,...`, `--target NAME` (the run's target or job), `--triggered-by
  manual|api|schedule|client` and `--since 24h` (or an RFC 3339 time or a date); they combine
  with AND, and `--limit`, `total` and `truncated` count the runs they keep. A failed run's row
  ends with its first failed step and error (`clean: ZeroDivisionError: division by zero`), and
  JSON runs gain `failure: {node, error}`. `barca history RUN_ID` shows one run: its fields, the
  schedule tick and `client_run_id`, every step it recorded with attempts, time, run hash and
  artifact, each failed step's error, traceback and how a dead worker ended (`SIGKILL (possibly
  the OOM killer)`), and a hint naming `barca logs RUN_ID --step STEP --stream stderr`. Against
  a server the filters are `GET /history` query parameters and the detail reads `GET
  /runs/{run_id}`, whose steps gain `partition_key`, `attempts`, `run_hash`, the artifact,
  `error_type`, `error_message`, `error_traceback` and `worker_exit`, and which gains `job`,
  `attempts`, `client_run_id` and `failure`. A run only this machine has says `barca history
  RUN_ID --local`. Cached steps are counted, not listed (`barca docs status`, "Finding and
  reading a failed run").
- Debug a pipeline from the CLI, against a server too. `barca status --problems` keeps the
  nodes that need attention (every state but `cached` and `always_runs`, a task or sensor whose
  last run failed, a schedule that is paused or failing) and `--state <states>` the named cache
  states; the summary still counts every node and the JSON names the filter in `filter`
  (`GET /nodes?state=&problems=` over HTTP). The table's WHY column shows the first line of a
  failed node's error (`failed: ZeroDivisionError: division by zero`). A scheduled node has
  `schedule` in `status --json` (`cron`, `next_fire`, `failures`, `max_failures`, `failing`,
  `paused`, `last_status`, `live`) and a SCHEDULE column: from a server, its scheduler's view,
  the numbers `GET /schedule` and the web UI show; on this machine, what the metadata DB holds.
  `barca doctor` checks a configured server (or `--server <url>`; `--local` skips it): its
  version and API version, its store against this project's `[remote]`, draining, files it
  failed to load, its scheduler and failing schedules, and the runs this machine has not posted
  to it, each failed check with what to do. `barca docs agents` has a new "Debugging pipelines"
  section (`barca docs status`, `barca docs doctor`).

- `barca sql` works with server mode. With a server configured (`[server] url`,
  `BARCA_SERVER`) or `--server <url>`, the server says which result each view reads from its
  ledger (the new `GET /sql/views`: the current result per node, its store path, format and
  digest, and a partitioned asset's current keys) and the query runs on this machine over the
  store, so a teammate queries results another laptop computed. View names, stale results and
  partition rules are a local `barca sql`'s; `--local` queries this machine's own records; an
  unreachable server exits 3 naming `--local`, with no fallback; a result this machine has not
  posted yet (unsynced) is named, not mixed in. barca's own records are queryable as
  `barca.runs` (`barca history --json`), `barca.materializations` (one row per step result; with
  a server, the new paginated `GET /materializations`) and `barca.nodes` (`barca status --json`,
  flattened), locally and from a server (at most 10000 rows of each, with a note), and `show
  tables` lists them. The server never runs SQL: DuckDB reads and writes files and the API has
  no authentication (`barca docs sql`, "With a server" and "barca's own tables").

- `--partition` works without a target (`barca get --partition retailer=r1,r2`: every asset and
  sensor, for those keys), on `barca run`, on `barca plan`, with `--server` (`RunRequest.partition`,
  now accepted for `run` too) and in `barca.get(..., partition=...)`, including with `server=`. The
  JSON results of `get`, `run`, `--dry-run` and `plan` gain `partition_selection`: the selection,
  each narrowed node's `selected` and `skipped` key counts and the `skipped_nodes`. A value that is
  a key of no node with that dimension, or a dimension no node in the cone has, is a usage error.
  Selecting a few keys plans only those keys: at 200,000 keys on a three-asset chain, planning 10
  keys takes 71 ms and 99 MB, against 378 ms and over 850 MB for every key (`barca docs
  partitions`, "Selecting keys").

## 0.29.2 (2026-10-22)

### Features

- `barca logs` is a way to debug a live pipeline, here or on a server. Steps' Python stderr is
  recorded with the run as well as their stdout: `logging`, `warnings`, `print(...,
  file=sys.stderr)` and the traceback of each failed attempt, still shown live as before.
  JSON lines gain `stream` (`stdout` | `stderr`) and `ts` (when the line was written, RFC 3339
  UTC with milliseconds); `--stream stdout|stderr` keeps one stream and `--timestamps` puts the
  time before each text line. Lines are kept as the run goes, and `barca logs -f` follows a
  running run (without an id, the newest running one) and exits with its outcome: 0, 130 when
  cancelled, 1 otherwise. With a configured server (or `--server`), `barca logs` reads the
  server's runs through the new `GET /runs/{run_id}/logs` and follows them over
  `/runs/{run_id}/events`; `--local` reads this machine's. A run records at most 16 MiB of
  lines, then one `[barca] log cap reached` line. The web UI shows stderr lines muted and each
  line's time. Native writes to file descriptors 1 and 2 and subprocess output are still not
  recorded (`barca docs logs`, "Debugging a live pipeline").
### Bug Fixes

- In a terminal, a line printed while the progress bar is drawn (`checking artifact store`,
  `fetched N cached artifacts`, a warning) is written above the bar instead of onto its line,
  which left a stray `0/2` bar frame on screen.
- With a server configured (`[server] url` or `BARCA_SERVER`), `barca get` and `barca run` no
  longer back up this machine's metadata database, with or without `--local`: the server is
  the ledger and backs up its own. A client's backups went into the server's backup set
  (`<store>/<env>/state/backups`), rotated the server's own copies out and could be restored in
  place of the server's.
- `barca status` shows the shape of a result held in a store that is a directory (`file://`),
  as every result is in server mode with such a store, instead of `? (artifact file not found)`;
  `--json` `shape` is the shape instead of `{"note": "artifact file not found"}`.

### Documentation

- A new guide, [Share Results With a Team](https://barca.sh/team-server/), sets up server mode
  in three steps: one committed `barca.toml`, `barca serve`, and `barca get` as usual. Every
  output sample and error message in it was captured from a real run of 0.29.1.

- Sample output on the site is re-captured on 0.29.1 (#503): the `barca status`, `barca history`
  and `barca list` tables on the workflow pages, Pattern 01 and the cron comparison show times
  with `UTC` (local time for `NEXT FIRE`), and the execution-controls page shows the retry count
  (`attempts=3`), the timeout error naming the step, and the real elapsed time in the progress
  line. The `barca docs status` example of a changed-bytes line ends its time in `UTC`.
  `DIRECTION.md` says what shipped from 0.24.2 to 0.29.1 and what is next.

- The README opens with one headline, links to the docs, guide, changelog and contributing
  guide, a CI badge for `main`, a docs badge and a pre-1.0 status note; a terminal image of a
  cold run, a cached run and `barca status` after an edit; a "Why barca" list with the
  benchmark numbers from `benchmarks/RESULTS.md`; and a screenshot of the web UI with runs in
  progress. The images are in `site/public/readme/`, linked by absolute URLs so they render on
  PyPI. CI now also runs on pushes to `main`, so the badge has a result to show.

- The release workflow deploys the docs site, barca.sh, from each `v*` tag once the GitHub
  Release and the PyPI upload are done, so the site matches the latest release instead of the
  last manual `npm run deploy`. Nothing waits on the deploy, so a failure never blocks a
  release. It needs the `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` repository secrets
  and fails naming whichever is missing.

## 0.29.1 (2026-10-21)

### Features

- Barca reports the environment variables a step reads without declaring them in `env=[...]`
  (#574). They are found statically, in the function and in the project helpers it reaches
  (`os.environ[...]`, `os.environ.get(...)`, `os.getenv(...)`, pydantic settings fields), and
  reported as one `[barca] warning:` summary line per command, an `undeclared_env` item per
  variable in the `warnings` array, and `undeclared_env` on each `get`/`run`/`--dry-run` step and
  `status` node (experimental). Run hashes do not change: switching such a variable still serves
  the cached result, and the warning names the `env=` fix.

### Bug Fixes

- With an artifact store, a `--refresh` killed after its upload and before its result was
  recorded no longer leaves the result `unsynced` (`mismatch`) with a warning on every `get`
  until another `--refresh` (#576). When a run fetches a store copy that is not the recorded one,
  the store's copy wins as before, and the run now also records its digest: the warning is
  printed once, and the next `get` and `barca status` find the result `cached`. With a server,
  its ledger keeps its record as before.

- A variable declared with `env=[...]` that is not set in barca's environment is now read from
  the project's `.env` (#575). A pipeline that loads `.env` at import (`load_dotenv()`) was
  hashed with the variable unset, so editing the value in `.env` served the old result; now the
  edit re-runs the asset, and `steps[].env` and the `--agent` line report the value from `.env`.
  The environment still wins over the file. Barca reads the file statically and does not set its
  values for the step.

### Performance

- A warm run with a store checks the store at most every 5 minutes (#593). A successful startup
  check is remembered in `.barca/gc-state.json`, per environment, store URI and storage
  options, and a `get` or `run` within 5 minutes of it starts no transfer helper unless it
  uploads or fetches something: no startup check and no check of its cached targets against
  the store. A run that does transfer starts the helper when it first needs it, and checks the
  store before its first fetch. A run after the window, another store or environment, pending
  uploads, `--refresh`, `--refresh-all` and `get -o url` check at startup as before, and a failed
  transfer ends the window. So a store object deleted behind barca's back is noticed within 5
  minutes rather than by the next run (`barca docs remote`, "How often the store is checked").
  On a 9-node project with two file sensors and a MinIO store on localhost (under `taskset`), a
  warm `barca get` took 216 ms with the store and 54 ms without before this change, and 43 ms
  and 42 ms after it: the store adds about 1 ms instead of 160 ms, with no helper started.

- A lazy input read from the store (`duckdb.DuckDBPyRelation`, `pl.LazyFrame`) fetches its
  byte ranges once per machine (#577). The ranges are kept in
  `.barca/range-cache/<run_hash>-<version>/`, keyed by the result's run hash and the store
  object's version, so a later run that reads the same ranges fetches nothing and a rewritten
  object is never served from stale bytes. The cache size limit counts the directories and
  evicts them with the leftovers, least recently used first.

### Testing

- The remote planning-pipeline test reads the whole-tree and largest-process peak RSS from the
  same /proc snapshots, so under load the largest process can no longer read above the tree
  (#595).

## 0.29.0 (2026-10-21)

### Breaking

- **Breaking:** `inputs=` keys and partition dimensions are checked against the function's
  parameters when barca reads the file (#549). A key that is not a parameter, or a parameter
  with no default that is neither an input, a partition dimension nor a `partitions_from`
  upstream, is a usage error (exit 2) naming the file, line, node and parameter, with "Did you
  mean"; `list`, `plan`, `--dry-run`, `get` and `run` all report it before any step runs.
  Before, it was a worker `TypeError` after the upstream steps ran, so a file holding such a
  step that was never run planned fine and now stops every command that reads it: fix the
  signature, or give the parameter a default. `**kwargs` takes any key; a `@task`'s parameters
  are not required (they are how `parallel()` branches get their arguments).

- **Breaking:** a retry after an exception may run in the same Python process as the failed
  attempt (#523): the worker that raised takes the retry, so module state (globals, caches,
  clients) carries over from the failed attempt instead of starting fresh. A step that needs a
  fresh interpreter per attempt should reset that state itself; a worker is still replaced after
  `sys.exit()`, `MemoryError`, a timeout, a failed module import, an unfinished `parallel()`
  exchange or a crash.

### Features

- Heavy steps can be kept from running all at once (experimental, #436). A new decorator
  argument `weight=N` (a whole number, default 1) on `@asset`, `@task` and `@sensor` says how
  much of the pool's capacity a step takes while it runs, and the steps running at once add up
  to at most the capacity: the pool size unless `[execution] capacity` in barca.toml or
  `BARCA_CAPACITY` sets it (the variable wins). Under a container's memory limit, three heavy
  assets with `weight=2` and a capacity of 4 run two at a time instead of three, and light steps
  still use what is left; outputs and cache decisions do not change (`weight=` is not part of
  the run hash). A step heavier than the capacity runs alone, with a warning. A heavy step
  waiting for room is not overtaken by lighter steps that became ready after it. Partition keys
  and `parallel()` branches take their node's weight. `barca plan` gains `pool_size`,
  `capacity`, `capacity_source` and `weights`, `--dry-run` gains `capacity` and `steps[].weight`,
  and `barca doctor` a `capacity` check. See `barca docs assets`, "Heavy steps", and
  `barca docs scheduling`, "Containers with a memory limit".

- "What did my last run do?" is one command (#555). `barca history` has a TARGET column between
  CMD and STATUS: the run's job by function name (`kpis`, `country_report,weekly_summary`), `all`
  for a run without targets, cut at 24 characters with `…`. History JSON gains `job` (additive):
  `all` or the resolved node ids, sorted and comma-separated, `null` for older runs; `--fields`
  accepts it, and `GET /history` carries it (OpenAPI specification 7). `barca logs` without a
  run id reads the most recent run and names it on stderr
  (`barca: logs of the most recent run: <run_id> (run notify, success, started ...)`).

- `barca status` and `--dry-run` say what changed in a stale node's run hash, not only that it
  changed (#558): `cache.changed` (status) and `steps[].changed` (dry run) list the parts that
  differ from the node's last successful materialization, `code`, `decorator argument NAME`,
  `decorator NAME`, `helper FILE:NAME`, `input ID`, `sensor ID`, `env NAME` or `kind`, and the
  WHY column and `detail` read `changed: helper pipelines/common.py:bucket_ltv (+1 more)`. Each
  successful materialization records a short digest per part in a new nullable
  `materializations.components` column (format 1, additive); a result recorded before keeps the
  plain `changed`.

- Backups say which machine took them (#553). Each sidecar records `host` and `project` (the
  project directory); `barca state backups` shows a HOST column and gains `host`, `project` and
  `mine` in JSON (`null` for a backup an older barca took, which is still listed and restored).
  Two machines backing up to one store, which is not supported without a server, now get one
  warning per command, on a backup or `barca state backups`, when another machine took a backup
  after this one's own, naming it and pointing at server mode. Pruning the store keeps the
  newest backup of every other machine taken in the last week (at most five), so one machine
  can no longer rotate out the other's last backup, and `barca state restore` without `--from`
  restores this machine's newest backup, saying so when newer ones came from elsewhere
  (`--from 1` is the newest). `barca state restore --json` gains `host` and `project`.

### Bug Fixes

- One byte formatter (#506): every size barca prints is in decimal units, as `[cache] max_size`
  is written (`KB` = 1000 bytes). The upload, fetch and backup lines, `barca state backups` and
  `barca sql`'s "fetched" note used binary units labelled `KB`/`MB` (`48.0 KB` for 49,152 bytes is
  now `49.2 KB`; `barca sql` printed `40 bytes`, now `40 B`). `barca status` shape samples now
  encode values as `barca sql` does: a NaN or infinite float is `null` (it made the shape reader
  fail), a datetime is ISO 8601 with `T`, a decimal a number, bytes hex, a duration seconds. One
  helper runs barca's JSON-in/JSON-out Python helpers, one runs a database transaction, and the
  Python file hash, identifier quoting and JSON conversion each have one copy; five unused
  functions are gone.

- A syntax error in a project helper module is reported when barca plans, not in the worker
  (#550). A command that plans a step of a file importing the module (directly, through other
  project modules, or inside a function: `get`, `run`, `--dry-run`, `plan`, `status`) exits 2
  before anything runs, with `syntax error in helpers.py: line 4, column 20: ...`, the step,
  and the source line with a caret; commands whose steps do not import it, and `barca list`,
  are unaffected. A `SyntaxError` the worker still meets on an import barca cannot see
  (`importlib.import_module`) shows the file, line and caret instead of importlib's frames.

- Naming one file of a package project works (#551): `barca run notify pipelines/reports.py`
  also reads the pipeline files the named file takes inputs from (`from pipelines.transforms
  import daily_revenue`, `asset_ref(...)`), transitively, and keeps only the nodes the named file
  needs from them, so `barca get pipelines/reports.py` does not run `transforms.py`'s other
  assets. Node ids and cache entries match a whole-project run. An absolute import in a file
  inside a package resolves from the project root, and an "is imported from" error names the
  file the import reaches, never a path like `pipelines/pipelines/transforms.py`. An input only
  the whole project can resolve (a bare name, a re-export through `__init__.py`) says to name
  the defining file or drop the file arguments.

- `barca state reset` and `barca state restore` no longer leak the cache (#552). The artifacts
  the old database named, which no record names afterwards, are now tier-1 leftovers: the cache
  size limit evicts them first and `barca gc --max-size off` deletes them (an unnamed file is
  kept while a run is in progress, or for a minute after it is written unless it predates the
  database). Before, any file under `.barca/artifacts/` that no result named was counted and
  never deleted, so the limit could never be met. The reset still deletes nothing: it says how
  much stays and how to delete it, and the undo works until the files are evicted (an evicted
  one is computed again). `barca gc --json` gains `held` (`what`, `bytes`, `evictable`: cached
  results, unreferenced files, metadata databases, the databases reset and restore moved
  aside, backups, other), and when it cannot reach its target it prints `held by:` instead of
  only "nothing to evict". The next check after a reset or restore measures `.barca/` again
  instead of trusting the last walk.

- A run held for its uploads and posted to the server later (#507) no longer prints "the
  function is not deterministic" for a hand-refreshed `Manual` asset when the server answers that
  it held other bytes: the held post keeps which of its steps barca checks for changed bytes, so
  the later post warns for the same steps as one sent at the end of its run. A post kept by 0.28
  or earlier still warns for every step it carries (#570).

### Performance

- A step that raises an exception no longer stops its worker (#523). The worker reports the
  failure with its traceback and takes its next step, so a failed partition key no longer costs
  a worker start (about 45 ms) and the rest of its batch is not sent again. With half of 10,000
  keys failing (`benchmarks/partition_scaling/`, `FAIL_HALF=1`, pool 8) a cold run takes 3.3 s
  instead of failing with "no workers available" after 96 s, and the coordinator sends 20,000
  steps instead of re-sending batches (570,000 by the time it gave up). A retry after an
  exception may now run in the same process, so module state carries over from the failed
  attempt. A worker is still replaced after `sys.exit()`, `MemoryError`, a timeout, a failed
  module import, an unfinished `parallel()` exchange or a crash, and a crash still names the
  exit signal.

- A warm run with a store no longer pays for work that changes nothing (#554). A sensor that
  returned what the store already holds at its location, by this machine's record of the
  confirmed upload, is not uploaded again. A run that recorded no new result takes no metadata
  backup at its end: nothing failed or was uploaded, every step it ran was such a sensor, and every
  earlier run is in a backup the store confirmed. Its run row goes with the next backup. The
  store is asked about all of a `get`'s cached targets at once rather than one by one. On a
  9-node project with two file sensors and a MinIO store on localhost, a warm `barca get` went
  from about 1.4 s to 0.5 s on a loaded machine (0.13 s without a store). What is left is the
  transfer helper's start and the startup check, which a `get` of a cached target still needs.

### Testing

- Three tests no longer fail by chance or by environment: the `barca state restore` refusal test waits for the server's lock to be released instead of assuming it is immediate (a child process started by another test briefly holds it) (#524); the Datadog parallel-span test counts each span once and only its own run's (ddtrace resends a payload it timed out on, and the fake Agent now serves requests concurrently) (#530); the coordination-socket tests build their fixtures under `/tmp`, so they pass when the runner's `TMPDIR` is long (#571).

## 0.28.3 (2026-10-21)

### Features

- The web UI shows which pipelines and assets are running now, whoever started the run (the
  scheduler, `POST /runs` from another client, `barca get --server`, or a `barca get` / `barca run`
  in a shell on the same `.barca/`): a running node pulses on the graph and the pipeline page,
  the assets list shows a Running badge with the elapsed time (keys done/total for a partitioned
  node) linking to the run, pipelines and groups count their running nodes, and the header shows
  "N runs in progress". `GET /state` and `GET /assets/{name}` gain a `running` field
  (`run_id`, `triggered_by`, `started_at`, `step`, `partitions`), and `GET /runs?status=running`
  lists the runs in flight with their `active_steps`. The UI polls `/state` every 2 s while
  anything runs (#534).

- A mistyped name gets "Did you mean ...?" (#556): a flag (`--refesh` -> `--refresh`), a
  subcommand (`statsu` -> `stats` or `status`), a target (`barca get amount` -> `amounts`), a
  `--refresh` name, a `barca sql` view and a `barca docs` topic, by the rule decorator arguments
  already used: the closest names within one edit (two for a name longer than four characters),
  the best match only, or two that tie. The suggestion starts the JSON envelope's `remediation`
  and has its own line in human mode; `error` and the exit code (2) are unchanged, and barca
  never acts on it. clap's misleading `use '-- --refesh'` tip is gone. This reverses the "no
  fuzzy suggestions" rule in `barca docs agents`.

### Bug Fixes

- Usage errors point at `barca list` only when the target is the problem (#492): an unknown or
  ambiguous target (and `barca run` without one) ends with ``Run `barca list <files>` ...``;
  every other usage error (positionals in the wrong order, `get` on a task, an unknown
  `--refresh` name, `-o url` without a cloud store, a removed setting such as `BARCA_STATE`, an
  invalid `barca.toml`) carries its own remediation instead. Through `--server` an ambiguous
  target's envelope now has the same remediation as a local one.

- Quieter first run (#496): the worker's `[barca] importing <module>` line is printed only under
  `--agent` (and in `barca serve`); the import time stays in the run JSON as `import_seconds`.
  The human tables of `status`, `history` and `stats` mark their timestamps `UTC`.
  `barca docs --pretty` is accepted (markdown, the default). `barca stats` labels its rate
  "Re-executed with unchanged inputs" and counts only the executions that repeated an earlier
  run hash; it was labelled "Cache hit rate" though cache hits are never recorded per asset. The
  JSON key is still `cache_hit_rate`.

- A run whose upload failed is posted to the server's ledger once a later command uploads it
  (#507). The run's post is kept `unsynced` (`barca status`: `not_in_ledger`) while its upload
  is pending, and the command whose retry uploads the results posts it before deciding its own
  steps, so that command and other machines get hits instead of computing it again. Before, such
  a run was never posted. A pending result dropped because its local file is gone is taken out
  of the post.

- A `Manual` node refreshed by hand now reaches what reads it (#548). Affects 0.28.0 and
  0.28.1: after `barca get fx --refresh fx` gave `fx` a new result for the same code and inputs
  (a reference table that reads external data, the documented use of `Manual`), an asset that
  reads it kept serving its old cached result, `barca status` said `cached`, and `barca serve`
  ticks did the same; only a `--refresh fx` with a target below it updated it. What reads a
  `Manual` asset now hashes its bytes as well as its run hash, as a sensor's consumers do, so
  it is `stale` after such a refresh and the next `barca get` or tick recomputes it; a refresh
  that gives the same bytes changes nothing below it. This holds for `--partition` and for a
  key of a partitioned `Manual` asset too. The run hash of every asset that reads a `Manual`
  asset changes once: each recomputes once after upgrading.

- Refreshing a `Manual` asset by hand no longer prints "produced different bytes for the same code
  and inputs; the function is not deterministic", and neither the run's `steps[].bytes_changed`
  nor `barca status`'s `last_bytes_change` reports it: a `Manual` asset is refreshed with the
  same code and inputs precisely to pick up new data (#548).

- A long `TMPDIR` (over about 80 characters, as CI runners and sandboxes set) no longer fails
  every run with `socket bind: path must be shorter than SUN_LEN` (exit 3). Barca's coordination
  sockets (workers, the transfer helper) now live in a private per-user directory,
  `$TMPDIR/barca-<uid>/` (0700), falling back to `/tmp/barca-<uid>/` when that path would exceed
  the platform's socket path limit (107 bytes on Linux, 103 on macOS); when neither fits, the
  error names the path, the limit and the fix. Sockets left by a killed coordinator are removed
  by the next run (#557).

- A sensor that returns a new value, or a task whose output changes, no longer prints "produced
  different bytes for the same code and inputs; the function is not deterministic", and
  `barca status` no longer keeps that event for it. The byte check now applies to assets only.

- The end-of-run line `[barca] N/M steps | done in Xs` shows the run's wall time. It showed the
  sum of the steps' times, so a run of parallel steps reported several times its real duration
  (`done in 8.2s` for a 1.9 s run).

- A failed step's message starts `Step '<node>' failed:` instead of `Worker failed:`, which read like
  a barca fault. Its remediation fits the failure: a timeout says to raise `timeout_seconds=`, a
  worker that died without a Python exception (`os._exit`, a native crash, SIGKILL) says so, and
  "see the traceback" appears only when there is one.

- Refusals that are not barca failures no longer end with "Not a problem in your code: barca or its
  environment failed. Retrying may succeed.": an unreachable or missing artifact store says what to
  check (URI, that the bucket exists, endpoint, credentials, `BARCA_REMOTE=off`), and
  `barca state reset` / `restore` refused under `barca serve` or a running run say what to do
  instead (the UI's Reset database button or `POST /state/reset`; stop the server; wait or
  cancel).

- A syntax error in a pipeline file names its line and column (`syntax error in pipeline.py: line
  5, column 18: Expected ':', found newline`) instead of a byte range (`at byte range 112..113`).

- `barca get` with no target names at most five of the tasks it skipped (`... and 37 more; barca
  list shows them`) and suggests `barca run <task>` without repeating every discovered file. On
  the 116-node fixture the line was 2,000 characters.

### Performance

- Small pipelines start faster (#560). Workers start together: when ready work has no idle
  worker, every worker it can use (up to the pool size) is started at once and each joins when
  it connects, where they started one at a time with a 30-40 ms wait for each; a one-step run
  still starts one. A worker is ready for its first step in about 25 ms instead of 50 ms: it
  imports `barca.api`, `barca.client`, `traceback` and `concurrent.futures` on first use, and
  hooks `logging` (to collapse repeated warnings) only once something imports it. Steps waiting
  behind the ready ones count when the batch size judges a workload trivial, so a fan-out whose
  streams start with a few cheap steps uses the whole pool from the start. A warm run (every
  step cached) opens the metadata DB once to start, once per phase and once to record, where it
  opened it eleven times for a three-phase pipeline: the run's start is one visit, the schema
  is read once per open, `last_read_at` is written in the ledger's transaction, unchanged cost
  estimates are not written back, and a run that wrote nothing skips the cache size check when
  the last check under the same limit passed. Pinned and interleaved, median of 15, before ->
  after: `trivial` 86 -> 47 ms, `deep_diamond` 297 -> 90 ms, `chain_100` 124 -> 97 ms,
  fan-out 500 x 50 ms 2.51 -> 1.82 s; warm `deep_diamond` 18 -> 13 ms. A worker's protocol
  hello now carries its `worker_id` (additive within protocol 3), and `BARCA_TRACE_TIMING`
  prints the metadata DB opens.

### Removed

- Committed junk and two broken examples (#497): the three `benchmarks/*/airflow/.venv`
  symlinks to macOS paths, the 0.1.5 macOS wheel under `benchmarks/partitioned_10k/wheels/` and
  `examples/chain_100/stderr.txt`. The Airflow harnesses take their venv from
  `BARCA_BENCH_AIRFLOW_VENV` (`benchmarks/README.md`); `examples/nested_tasks` and
  `examples/pathological_assets` load again (their `barca.toml` dropped the unsupported
  `[project]` table), and a test runs `barca list` in every example.

### Documentation

- `CHANGELOG.md` is the source of release notes (#504). The release workflow publishes the
  version's `## <version> (<date>)` section as the GitHub release body, from a `release-notes`
  job that the GitHub release and the PyPI upload both need, so a version without a section
  publishes nothing (`scripts/changelog-section.py`). `cliff.toml` and git-cliff are gone. Every
  section now uses `## <version> (<date>)` and the Breaking, Features, Bug Fixes, Performance,
  Removed and Documentation subsections, and the GitHub release bodies of 0.24.2 to 0.26.1 were
  rewritten from their sections.

- First-run pages show what barca prints today (#495): Getting Started, the Patterns pages and
  the CLI reference were re-captured on 0.27.0 (artifact paths with a small json preview,
  prefixed step output, no `importing` line outside `--agent`), their "run with barca 0.18.0"
  stamps are gone, `after=` and `when=` are documented as the usage errors they now are, and the remote
  store is described everywhere as this machine's backup and read-through cache, with sharing
  through a server.

- RFC statuses say what shipped (#502): the RFC template gains `Implemented (X.Y.Z)` and
  `Partially implemented (X.Y.Z)`; RFC-0007 is Implemented (0.27.0), with the route aliases'
  removal left; RFC-0008 is Implemented (0.28.0); RFC-0009, 0010 and 0011 are
  Implemented (0.26.0) and RFC-0012 Implemented (0.25.0); RFC-0006 is Superseded in part.
  `deploying.md` pins 0.28.0, and the guide, scheduling and core-constraints pages lose their
  0.18.0 stamps (re-checked, including a multi-file example that needed a qualified import).

- `barca docs` lists the manual in reading order (overview first, the contract last); the
  overview's Topics list and Commands table are complete (`logs`, `state`, `version`), and every
  topic, `cache` included, ends with its See also (#498).

- The README is the pitch, install, one example, the commands table and a "More" table that
  points at `barca docs <topic>` and the site; reference detail lives in the manual (#499).

- The guide's multi-file example imports `rows` as `from pipelines.sources import rows`; the
  bare `from sources import rows` it showed is refused (exit 2). The guide's sensor section no
  longer ends with a dozen paragraphs of dependency-tracking release notes, and the scheduling
  tables say that sensors are polled by every run that needs them, whatever their freshness
  (`Manual`, their default, does not pin a sensor).

- Benchmarks re-run on barca 0.28.0, Dagster 1.13.26, Prefect 3.9.0 and Airflow 3.3.2 on one
  Linux machine; `benchmarks/RESULTS.md` and the comparison pages carry the new numbers in place
  of the June ones, plus barca's trend across 0.1.5, 0.2.0, 0.24.2, 0.26.1 and 0.27.0 (#65).

## 0.28.2 (2026-10-21)

### Bug Fixes

- `barca[sql]` and `barca[test]` require `duckdb>=1.1,<2`. duckdb 2.0.0 (released 2026-10-21)
  removed `DuckDBPyRelation.fetchone()`, which `barca sql` uses to count rows, and refuses new work
  on a connection that has an unfinished result, which barca's shared worker connection and many
  pipelines that return DuckDB relations rely on. A fresh `pip install 'barca[sql]'` picked up 2.0.0
  and `barca sql` failed. Support for duckdb 2 is tracked separately.

## 0.28.1 (2026-10-21)

### Bug Fixes

- Concurrent `barca get` / `barca run` processes in one project no longer fail now and then with
  `Failed locking file '.barca/metadata.db'. File is locked by another process` (exit 3) (#537).
  Since 0.28.0, setting up the metadata DB released barca's cross-process lock before closing
  the database, so another process could open it while it was still held.

- An empty metadata.db is initialized, not refused (#537). A 0-byte `.barca/metadata.db` holds no
  history: a `touch`, a backup tool or a script that opened the path before barca created the
  database leaves one. `barca serve`, `get` and `run` now initialize it as a new database, with
  one line on stderr naming it (`[barca] note: the metadata DB <path> was empty (0 bytes);
  initialized it as a new database`), where a writer `serve` exited 3. A `--read-only` server
  reads it as no history yet and leaves it alone. A file that is not SQLite, a newer format or a
  malformed schema is still refused, and an empty `.barca/metadata.db` beside a named env's
  database still gets the stray-file warning.

## 0.28.0 (2026-10-21)

### Breaking

- **Breaking:** unscheduled `Always` nodes below a scheduled node, tasks included, now run at its
  ticks under `barca serve` (RFC-0008, #253): a tick of a scheduled node is one run that brings it
  and every `Always` node below it up to date. To keep a node out of its ticks, make it `Manual` or
  give it its own `Schedule`.

- **Breaking:** `barca get`, `barca run`, `POST /runs` (so `--server` too) and ticks no longer
  recompute a stale `Manual` node, upstream or the target of `barca get`, unless `--refresh` names
  it (RFC-0008, #253): they read its latest result and print
  `[barca] note: <name> is Manual and stale; ...`. Name it in `--refresh` (or use
  `--refresh-all`) to recompute it. A `Manual` node that has never been computed is a usage error
  (exit 2) naming that command. A `Manual` task named as the target of `barca run` runs. The older
  `POST /get/{target}`, `POST /run/{target}` and `POST /run` aliases are unchanged.

- **Breaking:** a writer `barca serve` whose metadata DB cannot be set up exits 3 before it binds,
  naming the file and what to do (upgrade barca, `barca state restore`, or `barca state reset`),
  where it used to keep serving with the scheduler off and one `scheduler disabled` line (#513).
  A database barca cannot use (empty, not SQLite, a newer format, a malformed schema) is now one
  error, `reason: "metadata_unusable"` in the envelope (new optional key; CLI and HTTP), instead
  of a raw SQL error such as `Parse error: no such column`; `get` and `run` on a 0-byte
  `metadata.db` now exit 3 instead of starting a new history in it. A `--read-only` server keeps
  serving and reports the error. `GET /health` gains `scheduler_enabled` and `metadata_error`.
  `serve`'s startup line is always printed and ends with `; metadata <path> (db format <n>)`, and
  a stray empty or invalid `.barca/metadata.db` beside a named env's database gets a warning
  naming both. A writer now waits for another process's DB lock (up to 60 s) before it binds.

### Features

- `barca serve` gives `Always` and `Manual` their run-time meaning (RFC-0008, #253). A tick of a
  scheduled node is one run with several targets: an asset is computed when it has no result for
  its current code and inputs, a task fires when its last successful run used other inputs (or a
  task it reads ran since, or it never ran). The tick's run keeps `triggered_by: schedule` and
  `scheduled_at`, and its `barca.run` span names every target. A failing scheduled node retries at
  every tick up to the new `Schedule(cron, max_failures=N)` (default 5), then is skipped and
  reported `failing` (`failures`, `max_failures`, `failing` in `GET /schedule`) until an explicit
  request or an input change; ticks skipped while a job is paused or still in flight do not count.
  Consecutive failures are kept in a new `schedule_failures` table, added in place within metadata
  format 1.

- **Reset the metadata database** from the web UI and with `barca state reset` (#508). A
  **Reset database** button on the "Can't load pipeline" panel and in the sidebar, behind a dialog
  that says what happens and how to undo it, calls the new `POST /state/reset`. The database and
  its `-wal` and `-shm` move aside to `metadata.db.reset-<UTC>` in the same directory and an empty
  one takes its place; nothing is deleted, artifacts stay, and a database that cannot be opened
  or migrated moves as it is. Refused with `409` (exit 3 on the CLI) while a run is pending or
  running, `403` under `--read-only`, `503` while draining. `barca state reset` asks in a
  terminal (`--yes` otherwise) and refuses while `barca serve` holds the environment. Undo with
  `barca state restore --from <the moved file>`: `--from` now also takes a database file's path.
  Experimental.

### Bug Fixes

- `barca serve` skips a cron tick while a `barca get` or `barca run` started from a shell on the
  same project (the same `.barca/`) is running the same node, as it does for a run started over
  HTTP (#479): the skip names the CLI run's id on stderr, in `GET /schedule` (`skipped_for`) and
  on the skipped `barca.run` span. A `running` row whose process is gone (a killed run) does not
  block. A run now records its job (`runs.job`) when it starts, not only when it ends.

### Performance

- Recording a run in the metadata database prepares each per-row statement once instead of
  re-parsing its SQL for every row (#460): the step rows the recorder writes during the run, the
  terminal ledger's success and failure rows, the read stamps (`last_read_at`/`evicted_at`),
  pending uploads, captured logs and the cost estimates, which now commit in one transaction
  instead of one per partition key. A cold three-level partition chain at 20,000 keys goes from
  12.0 s to 7.1 s, and the warm run from 3.3 s to 2.5 s. The rows written are unchanged.

- Skipping the dependents of failed partition keys no longer scans the ready queue once per
  skipped key (#461). When half of N keys failed while the rest waited, the coordinator touched
  the queue about N²/4 times (25 million at 10,000 keys); it now touches it about 3.5 N times.
  The ready queue keeps a membership set, so a skipped item is taken out in O(1) and its stale
  entry is dropped when it reaches the front. The skipped set is unchanged.

### Documentation

- The site's reference pages that are manual topics (CLI Contract, Discovery, Doctor, Groups,
  Remote Storage, Server Mode, SQL, Telemetry) are generated from `crates/barca-cli/docs/*.md`
  with `barca docs <topic>` references turned into links, and `cargo test -p barca` fails when one
  differs from its topic (#501). This brings back in step the pages that had drifted (`barca sql`
  reads CSV and evicts its `.barca/sql-cache/` copies; `get --json` always returns a pointer), adds
  Server Mode and Groups to the sidebar, and lists the Reference pages alphabetically.

- `barca docs cache` reads concept, behaviour, reference: what a run hash covers, what is not
  followed, the cached/stale/evicted/unsynced states, forcing a recompute, the size limit. Its
  0.18/0.19 upgrade notes moved to this file; its implementation guarantees to
  `specs/cache-decisions.md` (#500).

## 0.27.0 (2026-10-20)

### Breaking

- **Breaking:** `barca serve` drains on SIGTERM or SIGINT instead of cancelling its runs at once
  (#190). New runs get `503` with `Retry-After`, the scheduler stops, `GET /health` reports
  `"status": "draining"`, and runs in flight finish, uploads included, for up to
  `--drain-timeout` seconds (new flag; `[server] drain_timeout` in `barca.toml`; default 30). The
  server then exits 0. When the deadline passes or a second signal arrives first, the runs left
  are cancelled with reason `server shutdown` (in their logs, in `/status` `error`, and as
  `barca.reason` on the `barca.run` span) and the server exits 1, where it used to exit 0.
  For the old immediate cancel use `--drain-timeout 0` (it exits 1 when it cancels a run). Give
  the supervisor a stop grace period longer than the drain timeout (Docker's default is 10 s).
- **Breaking (HTTP API):** every error body is the CLI error envelope (`error`, `code`, `kind`,
  `remediation`, ...) instead of `{"error"}` alone (`error` is still there). `GET /assets` and
  `GET /assets/{name}` return the `barca list --json` node shape (`freshness` a lowercase word,
  cron in `schedule`) and the `barca stats --json` body (`id` instead of `node_id`).
  `GET /runs/{id}` returns the flat run view with its `outcome` instead of `{run, steps, logs,
  result}`. Run ids returned by the trigger routes are the database run ids. `POST /run`,
  `POST /run/{target}`, `POST /get/{target}`, `DELETE /run/{run_id}`, `GET /status/{run_id}` and
  `GET /events/{run_id}` stay as aliases for one minor release; use `POST /runs`,
  `DELETE /runs/{run_id}`, `GET /runs/{run_id}` and `GET /runs/{run_id}/events`. Event streams
  now end after `run_finished` and carry an `id:` per event.
- **Breaking:** Datadog dashboards and monitors need updating. The `<DD_SERVICE>-python` service
  is gone: `barca.execute` and library spans are in `DD_SERVICE`, and libraries are no longer
  patched unless opted in (`DD_PATCH_MODULES=<lib>:true` or `BARCA_TRACE_LIBRARIES=1`).
  `barca.run` resources use short names (`run publish`, was `run pipeline.py:publish`);
  `barca.step` and `barca.execute` resources are the node's short name (was the node id, with
  the partition key, and the job for `barca.execute`): filter by `@barca.node_id` or
  `@barca.partition` instead. The step tag `barca.node` is `barca.node_id`. Partition keys are
  `barca.partition` child spans of one `barca.step` per node. The `barca.serve.start`,
  `barca.serve.heartbeat` and `barca.serve.stop` traces are replaced by the `barca.serve.up`
  gauge (1, and 0 on stop) and its siblings: move monitors on those traces to the metric.
  Span `type` is `worker` (was `custom`).

### Features

- `barca serve` runs the server half of the `--server` run contract (RFC-0007 §3.4, #393).
  `POST /runs` takes the flags of `barca get` / `barca run` as JSON and answers with the run's
  database id at once (`dry_run` answers with the `--dry-run` JSON); targets are checked first,
  in the CLI's words. `GET /runs/{run_id}/events` streams every stderr line a local run prints
  (`line`, tagged `when: agent | always`), step output (`log`), step results (`step_finished`,
  with `cached: true` for cache hits) and `run_finished` with the outcome, each event numbered
  (`id:`); a slow or reconnecting reader receives every event once, and the stream ends after
  `run_finished`. `GET /runs/{run_id}` answers with the outcome (the exact `--json` body, or the
  failed-run body and error envelope) from the metadata DB, so it survives a restart;
  `DELETE /runs/{run_id}` cancels; `GET /runs/{run_id}/output` serves the final artifact.
  `GET /list`, `/nodes`, `/history` and `/stats/{name}` return the CLI's `--json` bodies byte
  for byte, `GET /project` describes the deployed tree, and `GET /health` adds `api_version`,
  `env` and `store_uri`. Runs record `triggered_by` (`manual`, `api`, `schedule`, `client`) and,
  for scheduled runs, `scheduled_at` and `schedule_id`, in `barca history --json`, `GET /runs`
  and the `barca.run` span (`barca.trigger`, `barca.scheduled_at`, `barca.schedule_id`, #434).
  The web UI uses the new routes and shows what started each run.
- `POST /schedule/{id}/pause` (optionally `?until=<RFC 3339>`) and `POST /schedule/{id}/resume`
  hold one schedule back without `--no-schedule` (#435). `GET /schedule` reports `paused`,
  `paused_until` and `skip_reason`; a paused job's ticks are skipped with reason `paused` (also
  on the telemetry span) and are not caught up. Pauses are kept in `schedule_state`, so a restart
  does not resume a paused job.
- `--server [<url>]` runs `get`, `run`, `status`, `list`, `history` and `stats` on a
  `barca serve` and prints the answer as the local command prints it: the same stdout JSON, the
  same stderr lines, the same exit codes, rendered by the same functions (#394, experimental).
  With no URL it uses `BARCA_SERVER`, then `[server] url` in `barca.toml`, and says so once on
  stderr; nothing configured is a usage error. `--local` ignores a configured server (and is a
  usage error with `--server`); `--server` on `serve`, `plan`, `sql`, `doctor`, `logs`, `docs`
  and `version` is a usage error. A run streams its progress and step output, and Ctrl-C cancels
  it on the server (a second Ctrl-C exits 130 at once). A server that cannot be reached, is not
  barca or speaks another API version is exit 3 naming its URL; a stream that drops mid-run is
  exit 3 naming the run id. A named file that differs from the deployed one prints a note.
  `get --server` prints the server's artifact paths (paths by default); `-o value` and
  `--partition` work as locally (`RunRequest.values`, `RunRequest.partition`), and `-o url` is a
  usage error with `--server`.
  `barca.get(..., server=True)` and `barca.run(..., server=True)` do the same from Python.
  `barca.Client` is deprecated in favour of them and warns when constructed. The binary now
  carries an HTTPS client (`reqwest` with `rustls`); `POST /runs` with `files` now reads only
  those files, as the CLI does. See `barca docs server-mode`.
- Server mode's ledger (RFC-0007 §3.4 and §4.1, #402), experimental. With `[server] url` in
  `barca.toml` (and `[remote]` pointing at the server's store), `barca get` and `barca run`
  still run on this machine but ask the server's ledger which run hashes exist
  (`POST /ledger/lookup`, one request for the plan), fetch the hits from the store, compute the
  misses here and, once their uploads are confirmed, post the run (`POST /ledger/runs`). The
  server checks every object in its store (a directory store by SHA-256, an object store by
  size) before it records the run as `triggered_by: client` with the laptop's run id in the new
  `client_run_id` (also in `barca history --json`); a missing or mismatched object is `409`
  naming the node and records nothing. A refused or undelivered post leaves the run a success:
  a warning names it, it stays `unsynced` in the new `ledger_posts` table and the next command
  against the server sends it again. A run some of whose uploads are pending (#390) is not
  posted. A run hash the ledger holds with other bytes prints the
  non-determinism warning once. A server that cannot be reached is exit 3; a different or
  missing store is exit 2. The ledger is reached over `http://` or `https://` with the
  `--server` client, from `[server] url` or `BARCA_SERVER`; `--local` runs against this
  machine's ledger and touches nothing on the server (error remediations name it). With a
  server configured, `barca status`, `barca history` and `barca stats` answer from the server
  (`[barca] server <url> (<source>)` once); `barca status` adds `local` per node (this machine
  has a copy) and shows a result whose post is pending as `unsynced` with
  `cache.reason: "not_in_ledger"` (`summary.unsynced` counts them). New manual topic:
  `barca docs server-mode`.
- `barca serve`: `BARCA_LOG=json` writes every line as one JSON object on stdout, with `ts`, a
  real `level` (`info`, `warning`, `error`), `msg`, the `run_id` and `node` it is about, and
  `dd.trace_id`/`dd.span_id` when the Datadog integration is on, so a container's logs join the
  run's trace and are no longer all filed as errors. A step's printed output becomes `info` lines
  of its run. Experimental; without it nothing changes, and `get`/`run` keep progress and errors
  on stderr (`BARCA_LOG` on another command prints a warning). `barca.step` spans gain
  `barca.upstream` (direct upstream ids) and, on a step that ran, `barca.digest` (#434).
- Datadog traces show the pipeline (#482). Every span reports as one service, `DD_SERVICE`
  (default `barca`), `type: worker`, `span.kind: internal`, with `env`, `version`,
  `barca.version` and `barca.project`. `barca.run` is named by the command and the targets' short
  names (`get daily_revenue`) and, on a failure, names the first failed asset and its error
  (`barca.failed_asset`, `error.type`, `error.message`). `barca.step` is one span per node, named
  by the node, with `barca.asset`, `barca.node_id`, `barca.file`, `barca.group` (nested `group()`
  path), `barca.freshness`, `barca.schedule`, `barca.cached` and outcome `skipped` for a planned
  step that did not run. A partitioned node is one step with `barca.partitions.total/executed/
  cached/failed` and a `barca.partition` child per key that ran (up to 200 per node, 2,000 per
  run) and per failed key. Python `barca.execute` spans are named by the node too. Library spans
  are opt-in: `DD_PATCH_MODULES`, `DD_TRACE_<LIB>_ENABLED`, or `BARCA_TRACE_LIBRARIES=1`
  (experimental). Trace ids are 128-bit (`_dd.p.tid`); traces go to the Agent's `/v0.4/traces`
  as JSON and are kept with sampling priority 2. `barca serve` reports `barca.serve.up`,
  `barca.serve.schedules`, `barca.serve.files` and `barca.serve.nodes` as DogStatsD gauges
  (`DD_DOGSTATSD_URL`, `DD_DOGSTATSD_SOCKET`, or `DD_AGENT_HOST`:`DD_DOGSTATSD_PORT`), with a
  Datadog event on start and stop.

## 0.26.1 (2026-10-20)

### Bug Fixes

- Metadata backups no longer overwrite each other when several directories back up to one
  store location in the same second (#487). A backup is now named by the UTC time to the
  millisecond plus a short suffix from the host name, the database's path and the copy's digest
  (`2026-10-14T18-02-11.482Z-3f9a1c2e.db`). `barca state backups` stays newest first and
  `barca state restore --from <name>` takes both these names and the to-the-second ones 0.26.0
  wrote, which are still listed and pruned.

### Removed

- Leftovers of the shared history: barca-core's `state_sync` module (its deadline and
  helper-error code moved to `helper_proc`; the WAL-fold wrapper only its own test called is
  deleted), and the cache manual's limitation about eviction marks in a pushed history. Docs,
  specs and comments no longer describe a shared-history push. No user-visible surface changes.

## 0.26.0 (2026-10-20)

### Breaking

- **Breaking:** a remote store fails soft once the steps are done (#390). When every step
  succeeded and only an artifact upload (or a metadata backup) failed, `get` and `run`
  exit 0 (was 3) with one warning, add `uploads: {pending, error, nodes}` to the run JSON and
  `--dry-run`, and record the results; `barca status` shows them `unsynced` / `local_only`
  (`summary.unsynced`) until the next command that opens the store uploads them first. Read
  `uploads` instead of exit 3 to detect unpublished results.

- **Breaking:** the shared history (`optimistic` mode) is removed (RFC-0009, #403; closes
  #243): `[remote] state`, `[remote] state_uri`, `[remote] push_retries`, `BARCA_STATE`,
  `BARCA_STATE_URI`, `BARCA_PUSH_RETRIES`, the pull before each run, the conditional push after
  it and the `metadata.db.prev` copy. A `barca.toml` or environment that still sets one is a
  usage error (exit 2) naming it and barca 0.26.0, even under `BARCA_REMOTE=off`: remove it. A
  single machine gets backups of its history with no setting (below); teams that shared history
  through a bucket move to a server (RFC-0007), and a wiped machine gets its history back
  with `barca state restore`. The old `state/metadata.db` object is ignored.

### Features

- With a remote store (`[remote] uri`), the metadata DB is backed up on the minute checkpoint
  and at the end of every `get` and `run`, and every minute it changed under `barca serve`, to
  `.barca/backups/` and `{uri}/{env}/state/backups/` (five copies each, with a `<UTC>.json`
  sidecar). New `barca state backups` lists them and `barca state restore [--from N|NAME]` puts
  one back after validating it, keeping the replaced database as `metadata.db.pre-restore`.
  A backup that fails to upload is a warning and `uploads.error`, never a failed run (#403).

- `barca status` with a store reports `unsynced` with `cache.reason` `store_only` and
  `mismatch` besides `local_only`, by comparing this machine's copy, the store object and the
  recorded digest; `barca get` repairs an `unsynced` target without running a step (upload,
  fetch, or re-upload with the changed-bytes warning and `steps[].bytes_changed`) (#403).

- `barca serve` runs with a remote store (it used to require `BARCA_STATE=off`) and backs its
  history up.

- Signed URLs (experimental, RFC-0011, #452): `barca.signed_url(uri, expires=3600)` returns a
  time-limited, read-only HTTPS link to any object in S3 (also R2 and MinIO), GCS or Azure, signed
  with the credentials barca already uses: SigV4 on S3, V4 on GCS (IAM `signBlob` when there is no
  key), and on Azure a service SAS with an account key or a user delegation SAS from
  `DefaultAzureCredential`. `barca get <target> -o url [--expires 30m|12h|7d|seconds]` prints one
  link per result (default 1 hour, at most 7 days); with `--json` each `_barca_artifact` gains
  `url` and `url_expires_at`. Links download as `<node>.<ext>`. The URL goes to stdout only.

- Slack notifications (RFC-0010, #450): set `BARCA_SLACK_WEBHOOK_URL` to an incoming webhook and
  barca posts when a job starts failing, when it recovers, and once a day while it keeps failing,
  for `barca get`, `barca run` and every run `barca serve` starts. `BARCA_SLACK_NOTIFY`
  (`changes`, `failures`, `all`) and `BARCA_UI_URL` (run links) tune it. A delivery failure is
  one warning that never names the webhook URL, and never changes a run's outcome. Runs now
  record their canonical job (`runs.job`) and the last post per job is kept in a new
  `notifications` table, both added in place within metadata format 1. The binary now carries an
  HTTPS client (`reqwest` with `rustls`), shared with the coming `--server` client. See
  `barca docs telemetry`.

- A cached result whose upload is pending is never evicted by the cache size limit, and
  `.barca/backups/` counts toward it but is never evicted. `barca get -o url` uploads a
  `local only` result before signing it.

## 0.25.1 (2026-10-20)

### Bug Fixes

- `barca status` reports a partitioned node's last materialization by a fixed rule: the latest
  run (by row id), and within it the first key in sort order. Before, it was whichever key
  finished last, so two identical runs could report different keys (#475).

- The release workflow's `verify-published-linux` jobs wait for the PyPI simple index to serve a
  new version before installing it, retrying every 20 s for up to 12 minutes (PyPI's CDN caches
  the index with `max-age=600`), so a release no longer needs a manual rerun for index lag (#480).

## 0.25.0 (2026-10-20)

### Breaking

- **Breaking:** `barca get` prints artifact paths by default (#404). In `--json` (and whenever
  stdout is piped) `final_output` is always the pointer `{"_barca_artifact": {"path", "format",
  "size_bytes"}}`, for json artifacts too, and a terminal shows the path with a small json value
  previewed under it. The old default, the value, is `-o value` (`-o value --json` for the JSON
  line); `-o path` prints only the paths. `barca.get()` still returns the value.

- **Breaking:** a partitioned target returns every key, sorted, never one key (#287):
  `final_output` is an object keyed by partition key (`"region=emea"`), and `barca.get()`
  returns a dict of key to value.
  `--partition region=emea,amer` (or a JSON object for compound keys) selects keys, computes only
  those and scopes `--refresh` of the target; `barca.get(..., partition=...)` does the same.

- **Breaking:** `.barca/` now has a size limit, `[cache] max_size` in `barca.toml` or
  `BARCA_CACHE_MAX_SIZE` (`"20GB"`, `"500MB"`, `"20GiB"`, bytes, `"10%"` of the filesystem, or
  `"off"`), defaulting to 10% of the filesystem `.barca/` is on. After every `get` and `run`,
  after every `serve` run and once a minute under `serve`, a cache over its limit is evicted
  down to 80% of it: leftovers (dead processes' staging, `*.moved-aside*` directories,
  `sql-cache/` copies), then superseded results, then local copies the artifact store holds,
  then the least recently read results; never the command's own outputs, results a run in
  flight holds, a storeless `Manual` node's current result, or the metadata databases. An
  evicted result is fetched or computed again under the same run hash when something needs it
  (step and status reason `evicted`). A project whose cache was larger than the limit sees more
  fetches and recomputes after upgrading; `max_size = "off"` restores the old unbounded
  behaviour. New, experimental: `barca gc [--dry-run] [--max-size SIZE] [--json] [--env ENV]`,
  the `cache` key on `get`/`run` results, the `cache_size` check of `barca doctor`, and `cache`
  in `GET /health`. `materializations` gains `evicted_at` and `last_read_at` (additive, metadata
  format 1); rows are never deleted (RFC-0012, #457).

## 0.24.3 (2026-10-20)

### Bug Fixes

- Open a metadata database last written by barca 0.21 to 0.23. Since 0.24.0, every command
  refused one with exit 3 ("its `materializations` table has no `bytes_changed` column"), and
  `barca status` failed with "no such column: bytes_changed". barca now adds the missing column
  in place, in one transaction, keeping every row; the older release still reads the result.

### Performance

- Cold runs of large partitioned pipelines scale linearly with the number of keys again: a
  three-level chain of 20,000 keys runs cold in about 9.5 s instead of about 245 s, with a
  0.55 GB coordinator instead of 5.3 GB (#345). Each key of a step used to wait on every key of
  the previous step in its stream through its own edge; one shared gate per step now keeps the
  same ordering. Dispatch also finds an upstream's output by id instead of scanning every
  output, and the end-of-run check no longer scans every item on each event.

## 0.24.2 (2026-10-20)

### Bug Fixes

- `barca serve`: a cron tick is skipped while any run of the same node is pending or running,
  including one started by `POST /get`, `POST /run/{task}` or `POST /run`, not only one the
  scheduler started (#432). The skip is logged with the run in flight, reported as
  `last_skipped` and `skipped_for` in `GET /schedule`, and sent to telemetry as a `barca.run`
  span with status `skipped` and `barca.reason: in_flight`.

- Refresh dependency lockfiles (`uv.lock`, `ui/`, `site/`, the dagster benchmark) to clear the open critical and high Dependabot alerts: PyJWT 2.15.1, aiohttp 3.14.5, cryptography 50.0.2, urllib3 2.8.0, oauthlib 4.0.0, astro 7.4.1, wrangler 4.151.3, react-router 7.18.4 and vitest 4.1.12, plus patched transitives.

- Failed `barca.step` telemetry spans now report their duration, from the first attempt to the
  permanent failure (retries included; a timeout reports about its timeout), instead of a
  zero-length span at the end of the run.

- A worker that dies while running a step (#433) now names how it ended instead of a bare
  `worker disconnected`: `worker for <node> exited with SIGKILL (possibly the OOM killer)`,
  another signal by name, or `exited with code N`, in the step error, the run JSON and the
  failure summary. `get`, `run` and `serve` print `[barca] env <env> (<source>); artifact store
  <store>` once on stderr at startup when `default_env` from `barca.toml` or an artifact store
  is in effect.

- Follow a project module reached through another module's `import` (`from wrapper import
  helpers` or `wrapper.helpers.f()` where `wrapper.py` does `import helpers`), and follow
  import chains at any depth instead of stopping six project modules deep; modules that
  import each other terminate. Editing such a helper now invalidates its consumers. Existing
  affected consumers may recompute once after upgrading.

## 0.24.0 (2026-10-15)

### Breaking

- **Breaking:** agent completion and permanent failure lines append `attempts=N` after
  retries. Allow this optional suffix when parsing those lines, or read the structured
  `steps[].attempts` field instead.

- **Breaking:** removed `barca get|run --no-cache` and `barca.get(no_cache=...)`.
  Use `--refresh-all` and `refresh_all=True`, respectively. The CLI alias is now an
  unknown-flag usage error (exit 2); Python rejects the removed keyword.

### Features

- Add `barca doctor` prerequisite checks, credential identity diagnostics and a documented local-only debugging profile.

- Run JSON reports executed-step durations and attempts, with slow module imports
  reported separately. History JSON totals recorded materialization attempts; stats
  retains per-materialization attempts. Timeout errors identify the step id.

### Bug Fixes

- Follow project helper references in exception handler types, including exception tuples
  and `except*`. Editing those helpers invalidates their consumers. Existing affected
  consumers may recompute once after upgrading.

- Follow project helper references inside set literals. Editing those helpers now invalidates
  their cached consumers. Existing affected consumers may recompute once after upgrading.

- Follow project helper calls in assignment receivers and indices, including annotated, unpacked, loop, context-manager and comprehension targets. Editing such a helper now invalidates affected cached assets.

- Follow project helper references in `for`/`while` `else:` blocks so edits invalidate affected cached assets.

- Follow project helper values, classes and mapping keys in `match` patterns so edits invalidate affected cached assets.

- Follow project helper references in ordinary function/class decorators, including nested definitions and decorator-factory arguments.

- Retain project-helper provenance for unpacked and augmented module constants, including aliases captured before later overwrites. Consumers conservatively hash the module source and static references.

- Track project star imports in ordinary helper modules conservatively, including functions/classes, captured constants, forwarded exports, multiple/conditional imports and transitive stars.

- Follow project helper references in function and lambda default arguments, including
  nested functions and keyword-only defaults. Editing those helpers now invalidates their
  consumers. Existing affected consumers may recompute once after upgrading.

- Retain project-helper provenance for names, functions and classes bound inside module-level `for`, `while`, `with` and `match` blocks. Consumers conservatively hash module source and static references.

## 0.20.0 (2026-10-08)

### Bug Fixes

- A run records the kernel it ran on (boot id), its pid namespace, its start time and a marker
  in `.barca/run-owners/`, and `barca history` reports it `interrupted` only on an observation
  that its process is gone (`specs/cache-decisions.md`); otherwise it stays `running`. A run
  started by 0.19.0 or earlier is still judged by its process id and host name, so in a
  container, on another machine or once its process id is reused it can stay `running`. 0.18.1
  reported a run killed by a restart of a machine whose project is on a network filesystem as
  `interrupted`, from the host name and process id; from 0.20.0 such a run stays `running`.

## 0.19.0 (2026-10-08)

### Breaking

- **Breaking:** the definition part of the run hash covers the decorator arguments that count
  (`barca docs cache`, "Which decorator arguments count") instead of the decorators' text as
  written. Up to 0.18 any edit there (a new partition key, a description, a comment, a
  reformat) re-ran the step for every key, and everything downstream (#283). Every step's hash
  is computed differently, so **every step recomputes once** on the first run after upgrading,
  in every project and every environment (`--env`); the run after that is served from cache.
  Results written by 0.18 stay in `.barca/metadata.db` and on disk under their old run hashes;
  0.19 neither reads nor deletes them. With an artifact store, 0.19 writes every result under
  its new run hash and leaves 0.18 results in the store alone; the two versions do not reuse
  each other's results. Run hashes in the JSON of `get`, `run` (also `--dry-run`) and `status`
  change value; no command, flag or JSON key changed.

## 0.18.0 (2026-10-07)

### Bug Fixes

- The run hash follows classes, imports inside function bodies, aliased `from` imports, modules
  used as values, and root modules imported from a subdirectory (#194). A step recomputes once
  after upgrading, with its downstream steps, if its function or a helper it reaches uses: a
  class defined in the project; a project module imported inside a function or class body;
  `from module import name as alias` of a project module; a project module as a value
  (`getattr(helpers, name)`, passing `helpers` along); from a pipeline in a subdirectory, a
  module in the project root; a helper module named like a pipeline file in another directory
  (both are hashed now); or, for a pipeline file inside a package, project modules imported as
  `from .helpers import f` or `from pkg.helpers import f` (these now resolve from the root, as
  they do when the step runs). A step whose helpers import the same name from two different
  modules outside the project (`from numpy import array` and `from jax.numpy import array`) had
  two possible hashes, picked at random on every run, and missed its cache about half the time;
  it now has one, and recomputes once if its last result was recorded under the other. Every
  other step keeps its hash and its cached results.

## 0.12.0 (2026-10-03)

### Features

- A sensor's output is part of the run hash of every asset that reads it (#188). Previously an
  asset reading a sensor was served from cache whatever the sensor returned. Assets that read a
  sensor re-run once after upgrading; pipelines without sensors keep their run hashes and
  caches. `--dry-run` and `barca status` predict from the sensor's last recorded output, and a
  sensor recorded before this version has none, so its consumers are `unknown` until it runs.

## 0.10.0 (2026-10-02)

### Breaking

- **Breaking:** `--refresh` cascades downstream by default: it re-runs the named assets and
  every asset downstream of them in the target's cone (#168). Previously it re-ran only the
  named assets and left their downstream assets cached; `--no-cascade` keeps that behaviour.

## 0.1.2 to 0.8.0 (summary)

### Breaking

- `barca run` is now cache-aware for upstream assets by default (same as `barca get`);
  the task itself still always re-runs. Previously every upstream asset was force-rerun.
  `--burst` is renamed to `--refresh <a,b>`; add `--refresh-all` (alias `--no-cache`) to
  restore the old default. Python API: `barca.run(..., burst=[...])` is now
  `refresh=[...]` / `refresh_all=True`.

### Features

- `--dry-run` on `barca get` and `barca run`: reports, for the exact command and flags, which
  steps are served from cache and which will run and why (`cached` / `run` / `partial` /
  `unknown`), without executing or writing anything. Real runs now report the same per step in a
  `steps` array (and `[barca] step:<id> cached` in `--agent` mode). The dry run and the real run
  share one decision function, and tests check that the predicted `will_run` equals the real
  run's `steps_executed`.

- `barca docs`: a manual compiled into the binary (concepts, output formats, caching, tasks,
  partitions, scheduling, runnable examples, conventions for scripts and AI agents). Every
  command's `--help` now ends with runnable examples, and `list`, `history` and `stats` take
  `--json`. Docs, help examples and JSON output are now part of the feature workflow
  (see CLAUDE.md); tests execute the manual's examples.

- DuckDB: steps returning a `DuckDBPyRelation` or a pyarrow `Table` are written as parquet
  without `serializer="parquet"` (a relation used to crash with "cannot pickle", a Table was
  silently pickled). Barca now owns one DuckDB connection per worker process and binds every
  duckdb-typed input as a view named after its parameter for the step, so SQL by name works
  in helpers without bind code; `barca.duckdb_connection()` exposes the connection for
  one-time configuration (extensions, credentials, settings). Steps that mix relations from
  their own `duckdb.connect()` with inputs still fail, but the failure now carries a `barca:`
  note explaining the conflict and the fix instead of only DuckDB's cryptic message.

- Sub-minute cron scheduling: `Schedule(...)` now accepts a 6-field cron with a
  leading seconds field (e.g. `*/15 * * * * *` — every 15 seconds); the `barca serve`
  scheduler evaluates at 1-second resolution. 5-field crons are unchanged (seconds
  pinned to `0`).

- Built-in cron scheduler in `barca serve`: enforces `@asset`/`@task`/`@sensor`
  `freshness=Schedule(...)`, timezone-aware (`--timezone`), durable catch-up on
  restart, self-overlap skip, and live status via `GET /schedule`; toggle with
  `--no-schedule`.

- Shared remote state and content-addressed artifacts: `barca.toml` config, `--env`
  environment separation, and remote artifact storage (Azure/S3/GCS/R2 via fsspec)
  with the metadata DB pushed/pulled as a blob.

- Minimal standalone scheduler example under `examples/scheduler`.

- Async-native core: the async runtime is owned by the caller, with cancellable runs.

- Centralized cron parsing behind a single `CronExpr::parse` helper so validation,
  the scheduler, and the `/schedule` handler share one grammar.

### Bug Fixes

- `--refresh` is no longer easy to misuse: a name that is not an upstream asset is an error that
  lists the valid names (it used to silently do nothing), `--refresh a b` says to use a comma
  instead of failing with "No such file", and barca warns when a refresh leaves cached assets
  downstream of it stale (run hashes cover upstream hashes, not outputs, so they stay cached).

- A step that runs for 15 s or more is now reported on stderr (`[barca] still running (45s): ...`,
  `BARCA_PROGRESS_SECS` to tune) in every mode, so a slow step no longer looks hung.

- Partitioned assets are now cached per key. Previously every partition re-executed on each
  run (a `TODO` in the coordinator); now unchanged keys are served from cache and only new or
  changed keys run, with the fan-in re-running only when its inputs change.

- Concurrent barca processes in one project no longer fail with `Failed locking file
  '.barca/metadata.db'. File is locked by another process`. Turso opens the DB
  single-process (its multi-process mode is experimental), and `barca get`/`run` held the DB
  open for the whole run. Barca now holds a short cross-process lock (`.barca/metadata.db.lock`)
  only while it reads or writes the DB and releases it while your Python runs, so processes
  queue instead of failing.

### Documentation

- Surface the task-scheduler workflow in the README and docs splash; new
  "Barca vs cron / systemd timers" comparison; sub-minute cron documented across
  the Scheduling guide and reference.

- Real `freshness=` (and other) keyword parameters on the `@asset`/`@sensor`/`@task`
  Python stubs for IDE autocomplete and type checking.

## 0.1.1 (2026-06-04)

### Features

- Correct __main__.py entry-point description in CLAUDE.md
- Clean up stale files from pre-Rust rewrite
- Polish: Result errors, default subcommand, versions, test isolation, README
- V0.1.1: Engine refactor + clap CLI + Python API

### Bug Fixes

- Fix 3 PR review issues: artifact key collision, binary caching, deterministic partition get

## 0.1.0 (2026-06-04)

### Features

- File-based artifact persistence: replace JSON-over-stderr with format-aware artifacts
- Require Python >=3.12, build wheels for 3.12/3.13/3.14
- Restore full release workflow from prior working config
- Use PYPI_API_TOKEN secret for PyPI publish (already configured)
- Engine hardening: refactor, protocol, first-class partitions, P0 fixes, CI/CD
- Rewrite gap tests: cross-file, sensor, partition (drop ops concerns)
- Add gap tests documenting known cache/staleness limitations
- Add cache fuzz tests: 100 random DAGs × mutate × verify staleness
- Add dependency cone analysis for staleness detection
- Implement `barca get` with cache-hit detection and staleness tracking
- Add cache/staleness integration tests (TDD: all currently fail)
- Add dagster server-mode partition benchmarks
- Add dagster/prefect for partition benchmarks, run full comparison
- Add partition benchmarks with partition-aligned stream assignment
- Implement dynamic partition eval and partitions_from resolution
- Implement static partition expansion and dynamic partition parsing
- Clean up DagNode duplication, remove dead code, tighten types
- Comprehensive planner tests documenting DAG shape → execution plan mapping
- Make parser pure: return Result, add 10 edge case tests
- Add integration tests for CLI behavior
- Add Airflow benchmarks: trivial, chain_100, deep_diamond, fan_out_500_50ms
- Complete benchmark fairness: server-mode dagster, parallel prefect
- WIP: Add server-mode benchmark scaffolding for dagster
- Address benchmark fairness concerns (ExSidius/barca#35)
- Add ETL pipeline, wide join, and incremental backfill benchmarks
- Add large_payloads, map_reduce, and multi_file_discovery benchmarks
- Add deep_diamond, wide_layers, and mixed_io_cpu benchmarks
- Wire up multi-process dispatch: workers communicate via stdout, Rust owns DB
- Add execution planner: Dag → decompose → Topology → plan → ExecutionPlan
- Add spaceflights, fan_out_500, and fan_out_500_50ms benchmarks
- Add benchmark results README
- Use SmallVec for inputs/sinks/partition_keys (stack allocation for small collections)
- Add comprehensive grammar spec tests, fix int parsing
- Rewrite barca as Rust binary with Python execution runner
- Add AGENTS.md (#33)
- Replace Datastar/Jinja2 UI with React + shadcn/ui (#31)
- Add comprehensive tests for multicore asset execution (#30)
- Add learning path to README, show artifact path after refresh, fix broken examples (#29)
- Replace plain text CLI output with Rich-formatted tables and panels (#28)
- Consolidate three packages into one + GitHub install instructions (#25)
- Update documentation with GitHub installation and CLI options (#24)
- Switch from sqlite3 to libsql as primary DB driver for MVCC concurrency (#23)
- Update documentation: reflect notebook workflow, add MkDocs, improve code docs (#22)
- Add notebook workflow helpers: load_inputs, materialize, read_asset, list_versions (workflow 7) (#21)
- Make sensors first-class nodes with dedicated CLI, API, and display (workflow 6) (#20)
- Add spaceflights benchmark: 10-asset diamond DAG adapted from Kedro (#19)
- Add test coverage roadmap with detailed spec for 100+ new tests (#18)
- Improve dependency tracking with codebase merkle hash, snapshots, and batch worker (#17)
- Add benchmark suite: Barca vs Prefect vs Dagster orchestration performance (#16)
- Refactor to align with barca.allium: freshness, sinks, run/dev/prune (#32)
- Release polish: metadata, changelog, serializer= parsing, __version__

### Bug Fixes

- Fix PyPI license metadata: use SPDX identifier instead of text table
- Fix all 11 PR review issues: CI, correctness, latent bugs, nits
- Fix all 3 staleness gaps: cross-file imports, sensor bypass, partition cascade
- Fix chain caching (ordered persist) + add cached benchmarks
- Fix run_hash consistency: 13/13 cache tests pass
- Fix: user print() statements no longer corrupt worker protocol
- Fix all clippy warnings, tighten idiomatic Rust
- Fix thread-safety in MetadataStore for concurrent per-thread usage (#27)
- Fix: make UI reactive to asset state changes after reset/reindex (#14)
- Fix: support bare @asset decorator (no parentheses) (#12)

### Removed

- Drop linux-arm64 from release matrix (simsimd NEON cross-compile failure)
- Remove dead code: plan.rs, NodeState, classify_shape, stats
- Remove AssetStatusBadge web component; use data-persist for dark mode (#15)

### Documentation

- Document Airflow 3 LocalExecutor limitations
- Document Airflow LocalExecutor limitation: requires PostgreSQL for parallelism
- Docs: fix datastar-reference for RC.8 syntax (#13)
- Docs: fix Quick Start so new users can actually run barca (#11)

## 0.0.3 (2026-03-14)

### Features

- Chore: use cross for Linux CLI builds in just release

## 0.0.3rc1 (2026-03-14)

### Features

- Ci: replace release workflow with local just release recipe
- Ci: use macos-latest for x86_64 macOS wheel (macos-13 unavailable)
- Chore: bump all crate versions to 0.0.3
- Ci: fix manylinux glibc compatibility for bundled CLI binary (#10)
- Init (#1)
- Initial commit

### Bug Fixes

- Fix: use command -v to check for cargo-zigbuild
- Fix: use uv tool install ziglang + symlink to zig in setup recipe
- Fix: ignore untracked files in dirty check
- Fix: handle PEP 440 rc versions in just release (convert to semver for Cargo)
