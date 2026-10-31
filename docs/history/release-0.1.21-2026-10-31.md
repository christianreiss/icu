# Release 0.1.21 — explizite PipeWire-Gerätewahl

Linux-Hotfix für `defined target not found`: Ein angeschlossenes Mikrofon
konnte ausfallen, wenn es zugleich explizit in ICU ausgewählt und das
Systemstandardgerät war. WirePlumber überschrieb die Gerätewahl mit
`target.node=-1`; der Schutz gegen unerwünschten Geräterückfall führte
anschließend zum Löschen des Streams.

Explizite Capture-, Wiedergabe- und Referenzstreams setzen zusätzlich
`node.dont-move=true`. „Systemstandard“ bleibt beweglich; Hotplug und die
Geräteauswahl werden weiterhin von ICU behandelt. Keine globalen
WirePlumber-Regeln oder Gerätestandards werden dafür verändert.

Der neue native Regressionstest ist bewusst opt-in, öffnet zwei Sekunden
lang das angegebene Standardmikrofon und verwirft alle Samples. Der
ursprüngliche Fehler wurde mit dem C-Media-USB-Mikrofon auf biest vor dem
Fix in 0,06 Sekunden reproduziert; mit dem Fix bestand derselbe Test in
2,04 Sekunden. Die beiden nativen Tests für Capture-/Referenzkanäle und
laufenden Kanalwechsel sowie Rustfmt und Linux-Clippy mit `-D warnings`
bestanden ebenfalls. Paketbau und Auslieferung werden nach dem Build
separat dokumentiert.
