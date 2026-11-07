# Release 0.1.25 — Accessibility and state fixes from the UX/UI review

The Fleet slop review of the whole UX/UI (job over the egui client and the
marketing site, four full runs) produced these changes. Commit `431aef9`.

## Changes

- **Client:** status pills and badges expose their text to assistive
  technology. The push-to-talk mode button has its own full-width row, so the
  complete binding is readable at the 400 px main window; the action icons sit
  on a second row (the bottom panel is about 50 px taller).
- **Site:** no distribution is recommended when detection fails; the
  selector keeps keyboard focus; reveal content fails open without `site.js`;
  a failed copy reports the failure and keeps the full selectable checksum
  until dismissed; a pause control stops continuous decorative motion; the
  download selector is a labelled group of pressed-state buttons; hero and
  showcase use current captures of the UI test harness, and the camera tab
  labels its older image.

## Review state

The last review run still disagreed on four P2 items. Three were fixed
afterwards. The fourth, a clipped refused-passport capture, is not a layout
bug: `ui_layout` shows the heading and all three verdict lines inside the
560×600 window with about 10 px of margin.

Not covered by the review or by tests: screen-reader behaviour, Windows and
Safari rendering of the site, update-prompt and Pro-offer captures, an
AccessKit export, and live (non-fixture) window screenshots.

## Build and delivery

Filled in after the build and publication.
