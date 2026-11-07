# Release 0.1.26 — Single-row main window controls again

0.1.25 always put the send-mode button on its own row, which made the bottom
panel about 50 px taller and moved the icons to the left even in the everyday
Automatisch state. The controls are a single row again (status left, icons
right) and only stack when the title or hint does not fit beside the icons,
for example a long push-to-talk binding like `CTRL+SHIFT+F12` or the hint
"Sendet, wenn du sprichst". Nothing is truncated either way.

Rendered with the `ui_layout` harness: Automatisch alone in the room keeps one
row; push-to-talk, and Automatisch with other people present, stack.

## Build and delivery

Filled in after the build and publication.
