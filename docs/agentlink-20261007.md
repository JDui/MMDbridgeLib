# AgentLink

AgentLink follows AXi-FaceMixer's local session workflow. The left panel contains
an editable, copyable Prompt; the right panel shows identity, progress and bounded
operation history. The page uses the existing glass surface, typography and motion
preferences. It does not start an external Agent.

The portable folder contains `MMDbridgeLib.exe`, `mmdbridge.exe` and
`skills/mmdbridge-card-manager/`. Open AgentLink, select an enabled directory and
asset type, copy the Prompt into an Agent that can read local files and run the CLI.
The desktop and CLI share `data/library.sqlite3` next to their executables.

Core owns the local file bridge, scope checks, tags, preview-preserving card sync
and persistent log. A session has a rotating ID/token and heartbeat. No network
listener or arbitrary command execution is added. Opening the page creates the
bridge lazily. During takeover, scope changes are rejected. Cancellation blocks
further work; an already committing operation may finish and its changes remain.
Starting a new session invalidates queued requests from the old session.
Each live CLI command includes the Prompt's `--session` ID. Old Prompt commands
cannot reconnect or write into a replacement session, even if its Agent name and
asset scope are identical. Edited Prompts must be regenerated after session changes.

Live tag writes preserve existing sources/confidence and manual removals. Current
previews are required, the bridge never scans or renders, and completion checks
that cards modified during the task have been synchronized. The page refreshes
assets on changes/completion, including while the AgentLink page is hidden.

Core protocol tests use temporary, explicitly marked preview contract fixtures
without a GPU. The separate probe renders a synthetic model and runs a real CLI
subprocess against the bridge. Screenshots use the actual frontend with recorded Core snapshots
and an isolated presentation bridge. They are not Windows desktop screenshots,
real-library tests, or evidence that an external Agent classified the models.
The fixture database and session token are excluded from review artifacts.

Working sessions add a restrained glass glow/sweep to both panels, an orbiting
operation glyph, progress sheen and short entry transitions for new log records.
The glyph reflects the most recent inspection, tag or card-sync operation.
Only the most recent six arriving records animate, and their classes expire;
restored history never enters again. Progress uses the actual reported value,
or an indeterminate track without a percentage when no value was reported.

Continuous effects stop on cancellation or a connection-read failure, pause
when the page/window is hidden, and resume after recovery. Both the appearance
motion switch and system reduced-motion preference disable animations. Completion
adds one brief green settle, with no replay on unchanged polls or navigation.
Loops animate decorative transforms/opacity, with no per-frame JavaScript,
animated blur, moving text, extra application dependencies or synthetic progress.

The isolated frontend review checks motion advancing without layout movement,
actual/unknown progress, bounded arrivals, history replay, copy interactions,
reduced motion, hidden pages, cancellation and connection loss. It records an
MP4 of the actual CSS using a synthetic Core trace and captures light/dark
working states and completion. This records presentation behavior, not Windows
device performance or an external Agent actually classifying user assets.
