---
status: done
---

# Sort a partial Result on the server from the header click

## Specs

- [Query execution](../specs/query-execution.md)
- [Results and editing](../specs/results-and-editing.md)

## Problem Statement

Clicking a column header on a Result that still has rows on the server does not sort it. The click only opens a bar that says "Not every row is loaded: sorting needs the server" and waits for a second click on **Sort on server**. The user expects the header click to apply the order to the query and see the whole Result ordered, but instead sees the rows unchanged until they find and press the bar's button.

## Solution

A header click on a partial Result reruns the query with the server's ordering at once. The three-state cycle (ascending, descending, unsorted) still works: the second and third clicks rerun with the new order, and the unsorted step reruns without an order. A complete Result keeps the local sort, which already covers every row and needs no roundtrip. The confirmation bar disappears.

## User Stories

1. As a sqlr user looking at a partial Result, I want a header click to order the whole query on the server, so that I see every row ordered and not only the loaded page.
2. As a sqlr user, I want the first header click to sort ascending, so that the direction is predictable.
3. As a sqlr user, I want a second header click to sort descending, so that I can reverse the order without another control.
4. As a sqlr user, I want a third header click to remove the server ordering, so that the Result returns to its natural order.
5. As a sqlr user looking at a complete Result, I want the click to keep sorting locally and instantly, so that small Results do not cost a query per click.
6. As a sqlr user, I want a server sort to combine with the SQL `WHERE` filter already typed in the filter bar, so that the rerun keeps the rows I narrowed.
7. As a sqlr user with pending changes, I want a server sort to be refused with "Apply or discard the pending changes first", so that my changeset is never replaced silently by a rerun.
8. As a sqlr user, I do not want to see a "Sort on server" bar any more, so that there is one obvious way to sort.
9. As a sqlr user, I want the sort arrows in the header to keep reflecting the ordering in force after the rerun, so that the next click continues the cycle from the right state.
10. As a sqlr user, I want the sorted rerun to appear in Messages like any other statement, so that I can tell the query ran again.
11. As a sqlr developer, I want the grid to keep queuing a `GridAction` and leave the rerun to the app, so that the widget still performs no engine work.
12. As a sqlr user with a Result whose query cannot be wrapped by the Dialect, I want the existing "No SQL dialect for this connection" notice instead of a silent no-op, so that I know why nothing happened.

## Implementation Decisions

- The Result grid's non-shift header click computes the next `(column, descending)` order with the existing cycle.
  - When the Result is partial (`more_available`), it queues `GridAction::SortOnServer(order)` immediately and does not change the local `sort` state. The app reruns, and the new grid is seeded with the server order so the arrows show it.
  - When the Result is complete, it keeps setting the local `sort` and queues `GridAction::SortOnServer`'s existing local counterpart, `GridAction::Sort`.
- Remove `GridState::server_sort`, the `server_sort_bar` widget and its call. No other state depends on the pending server sort.
- No change to `GridAction`, to the app's `server_rerun`, or to `Dialect::wrap_order`/`wrap_where`: the app already wraps the original statement, combines sort and filter, seeds the new grid's `sort` and `where_filter`, and refuses the rerun while a Changeset has pending changes.
- No change to document Results: their own table view keeps its paging and sorting actions.
- A shift+click on a header still selects the whole column; it never sorts.

## Testing Decisions

- Seam: the existing Result grid UI test harness in `crates/sqlr-ui/tests/grid_edit.rs`, which drives real header clicks through `egui_kittest` and inspects the queued `GridAction`s.
- Update the partial-Result test to assert that one header click immediately queues `SortOnServer([(0, false)])`, leaves the view unsorted, and leaves no "Sort on server" label on screen. Rename it to describe the click, not the bar.
- Keep the complete-Result test (`header_click_sorts_complete_result`) as the proof that the local path and the three-state cycle are unchanged.
- No new app-level test: the app side (`server_rerun` and the Dialect wrappers) is unchanged, and the behavior change is entirely in which action the grid queues and when.

## Acceptance Criteria

- [x] A header click on a partial Result queues `SortOnServer` with ascending order without any second click.
- [x] The second click queues `SortOnServer` descending and the third queues it with no order.
- [x] A header click on a complete Result still sorts locally through `GridAction::Sort` and the local cycle.
- [x] The "Sort on server" bar, its message and its buttons no longer exist.
- [x] The server rerun still combines the order with the filter bar's `WHERE` condition.
- [x] A rerun is still refused while the Result has pending changes, with the existing message.
- [x] The sort arrows reflect the server order after the rerun.
- [x] Specs and the manual describe the new behavior.
- [x] `cargo nextest run -p sqlr-ui -p sqlr-engine -E 'package(sqlr-ui)'` and `make test-fast` pass.

## Out of Scope

- Sorting document Results (MongoDB, Elasticsearch, Redis) differently; their existing views stay as they are.
- Sorting on the server when the Result is complete.
- Changing the wrapping SQL, the Dialect contracts or the server-side column ordering (including `NULLS LAST`).
- Changing the three-state cycle to a different interaction.

## Further Notes

- The local sort of a complete Result and the server sort of a partial one must keep agreeing on direction and NULL placement: NULLs last in both. The local implementation already sorts NULLs last; the wrappers add the database's `NULLS LAST` form.
- The change removes a two-step interaction but keeps the rerun refusal for pending changes, so applying edits is still never lost.
