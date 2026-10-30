# KDE Wayland tray activation

`ksni-0.3.6` and `tray-icon-0.25.1` are the published crates of those versions,
with only the activation-token additions listed below. Their original licenses,
package metadata and source remain included. Cargo cache metadata and lockfiles
are omitted. These path overrides can be removed when upstream exposes the same
notification end to end.

* ksni: implement the KDE `ProvideXdgActivationToken(s)` D-Bus method and forward
  its bounded string to a new, default no-op `Tray` callback.
* tray-icon: forward that callback as `TrayIconEvent::ActivationToken`; include
  the variant in `id()`.
* ICU: consume the one-use token for the next explicit show action, expiring it
  after ten seconds. Never log tokens. The native event loop restores/quits even
  when the compositor supplies no redraw callbacks to the minimized window.

Protocol: [KStatusNotifierItem interface](https://invent.kde.org/frameworks/kstatusnotifieritem/-/blob/master/src/org.kde.StatusNotifierItem.xml).
Plasma also supplies a token before a tray **menu** action; see
[`PlasmaDBusMenuImporter::actionActivated`](https://invent.kde.org/plasma/plasma-workspace/-/blob/master/applets/systemtray/statusnotifieritemsource.cpp).
No KWin scripting, desktop input injection or activation-policy bypass is used in
ICU. Native acceptance scripts use an isolated profile and control only their
own test window; desktop interaction is a test, not the implementation.
