# 0091: Compact automatically at 400K by default in the desktop

- Status: Accepted
- Date: 2026-11-24

## Context

ADR-0080 moved the compaction controls into the composer's **Chat settings**:
an **Auto-compact** checkbox and a slider from 128K to 1M tokens. When no chat
preferences are saved, the desktop took the CLI's values: auto-compaction off
and core's `DEFAULT_COMPACT_THRESHOLD` of 200,000 tokens. With auto-compaction
off, a turn whose context reaches the trigger stops before the model request
and asks the operator to compact, so a long desktop session stopped until the
operator found the checkbox. The operator asked for auto-compaction on and a
400K threshold by default.

## Decision

1. The desktop's built-in chat preferences (`desktop::preferences::built_in`)
   set `auto_compact` to `true` and `compact_threshold` to 400,000 tokens.
   They apply when no preferences file exists, and when a damaged file is
   replaced on the next save, as before.
2. A saved preferences file keeps its values. Nothing rewrites a stored
   `auto_compact: false` or a stored threshold; stored settings keep their
   meaning.
3. The default is a desktop constant, not a change to core's
   `DEFAULT_COMPACT_THRESHOLD`. A compile-time assertion keeps it inside the
   slider's range and on its 8,000-token step from 128,000, so the composer
   can show and save it unchanged: the composer rounds a value between steps.
   The step is a Rust constant used only by that assertion; it must stay equal
   to `COMPACT_THRESHOLD_STEP` in the composer, which the slider uses.
4. The CLI is unchanged: `chat` keeps `--compact-threshold` at 200,000 and
   `--auto-compact` opt-in.
5. Compaction still triggers at `min(threshold, safe input)`, so a model with
   a smaller safe input compacts earlier than 400K, as ADR-0080 describes.

## Alternatives considered

- Change core's `DEFAULT_COMPACT_THRESHOLD` to 400,000: rejected. It would
  change the CLI default and the core tests' meaning for a desktop request.
- Make the CLI compact by default too: rejected for this change. It would need
  a new `--no-auto-compact` flag, and the request named the desktop controls.
- Migrate saved files to the new defaults: rejected. An operator who turned
  auto-compaction off would have it turned back on silently.

## Consequences

- A new desktop install, or one without saved chat preferences, compacts
  automatically once the context reaches 400K tokens or the model's smaller
  safe input. Compaction sends a summary request to the selected model, which
  costs a model call that did not happen by default before.
- An install with saved preferences sees no change until the operator moves
  the slider or ticks **Auto-compact**.
- The desktop and CLI defaults now differ; both are documented.

## References

- [ADR-0080](0080-move-chat-budgets-to-settings-and-add-workspace-instructions.md)
- [Context management](../context-management.md)
