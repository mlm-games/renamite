## v0.3.11

- repose bump
- misc.
- artboard clip
- renamite mcp: shared ops, control channel, remote mode
- renamite mcp: stable node handles
- renamite mcp v2: timeline, clips, machines
- pathfinder in behavior-common
- renamite mcp server
- warp solve cleanup
- pin warp modifier
- variable-width strokes
- track live touch pointers by id
- Update Cargo.lock
- Update Cargo.lock
- replace renamite-text with repose


## v0.3.8

- restore the distinct keyframe glyph, the outline font has no filled circle
- derive every timeline hit test from the painted layout
- Revert "align timeline labels with canvas rows and make the record chip a toggle"
- Revert "point at the touch add-key routes in the empty timeline hint"
- point at the touch add-key routes in the empty timeline hint
- align timeline labels with canvas rows and make the record chip a toggle
- density scale the timeline chrome
- size every canvas marker in dp and drop the double-tap finish
- use one fill-varying glyph for the keyframe states
- show keyframes regardless of layer folding, mark keyed properties
- draw the pen preview curve and give the pen a way to finish
- add a overflow button since long press cannot be done on layers (they lose the drag func.)
- pin the zoom anchor and stop rail drags reaching the canvas
- zoom and pan finger center move fix
- make Escape close the number editor and bound its focus polling
- let a two-finger gesture take the pointer from a touch drag
- revert the name field sizing, it was not a defect
- clip inspector values to their field and grow the fields a little
- make the layer rename field focusable on touch
- let the focused panel own touch gestures, and keep finger drags off the zoom
- pin the number field's width instead of sizing it to its text
- let a field share its owner's focus cell
- centre the icon buttons in their rows
- clip the timeline canvas to its key area
- reserve the top app bar's height in the shell body
- pan, zoom and context-menu the timeline
- stop the compact tool palette above the viewport controls
- give the inspector's number fields the shared text field
- keep the compact tool palette to the left edge
- gate remaining persistent hints behind the same flag
- gate on-screen hints behind a flag
- two-finger canvas gestures and touch long-press context menu
- scroll the tool rail and palette, and pin the palette to the stage
- insets the shell body by the scaffold bars


## v0.3.7

- add rlobkit init line (missed)
- hide status bar


# Changelog

## v0.3.6

- cargo update
- fmt
- web: fix
- cargo update
- use the better textfield properties already in repose, and dnd for collapsable containers
- fix dropdown issue
- partial timeline shortcuts
- fix(ci): namespace the crate-publish concurrency group
- to preserve tooltip pos after recomp.

