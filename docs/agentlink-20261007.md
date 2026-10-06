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
