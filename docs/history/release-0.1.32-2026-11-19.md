# ICU 0.1.32 — neue Download- und Update-Domain

- Die Website und der Download-Hinweis im Client zeigen auf `https://icu.uggs.io/`.
- Der Client liest den signierten Update-Feed unter `/downloads/latest.json`.
  Der Release-Uploader verwendet `/api/v1` auf derselben Domain.
- Die Veröffentlichung von 0.1.32 im alten und neuen Feed ermöglicht den
  Übergang für bereits installierte Clients. Der alte Downloadpfad muss bis
  zur Ablösung der Altclients Manifest und Pakete direkt über HTTPS ausliefern.
- Windows (Setup und Portable) behält das bestehende Update-Verfahren. Unter
  Linux erfolgt die Installation wie bisher über das passende Paket.
