<?php
// ICU download page, rendered from what is actually on disk:
// downloads/latest.json (the channel the updater uses) and the manifests of
// every downloads/<version>/ directory. Nothing here is edited per release.

declare(strict_types=1);
require __DIR__ . '/_lib/release.php';

header('Content-Type: text/html; charset=utf-8');
header('Cache-Control: no-cache');
header("Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'");
header('X-Content-Type-Options: nosniff');

function h(?string $s): string
{
    return htmlspecialchars((string) $s, ENT_QUOTES | ENT_SUBSTITUTE, 'UTF-8');
}

function mib(int $bytes): string
{
    return number_format($bytes / 1048576, 1, '.', '') . ' MiB';
}

function file_row(array $f, string $release): string
{
    $flags = '';
    if ($f['version'] !== null && !$f['version_matches']) {
        $flags .= ' <span class="flag" title="Paketversion weicht vom Release ab">Version ' . h($f['version']) . '</span>';
    }
    if (!$f['present']) {
        return '<li><span class="row off"><span>' . h($f['label']) . $flags
            . ' <span class="flag">fehlt</span></span><span class="size">' . mib($f['size']) . '</span></span></li>';
    }
    return '<li><a href="' . h($f['url']) . '" title="' . h($f['name']) . ' · SHA-256 ' . h($f['sha256']) . '"><span>'
        . h($f['label']) . $flags . '</span><span class="size">' . mib($f['size'])
        . ' <span aria-hidden="true">↓</span></span></a></li>';
}

$releases = icu_releases();
$latest = icu_latest();
$current = $latest ? ($releases[$latest['version']] ?? icu_describe($latest['version'], $latest, '')) : null;
$older = array_filter($releases, fn($r) => $current === null || icu_version_cmp($r['version'], $current['version']) < 0);
$serverNote = (string) icu_config()['server_note'];
?><!doctype html>
<html lang="de"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="robots" content="noindex"><title>ICU — Downloads</title>
<style>
:root{color-scheme:light dark;--bg:#fafaf8;--fg:#20221f;--muted:#62665f;--line:#dedfd9;--hover:#f0f1eb;--warn:#a15c00}*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.6 system-ui,sans-serif}main{max-width:580px;margin:12vh auto;padding:0 24px 40px}header{display:flex;align-items:baseline;gap:14px;margin-bottom:24px}h1{font-size:36px;letter-spacing:-1.5px;margin:0;font-weight:650}h2{font-size:13px;font-weight:500;color:var(--muted);margin:0 0 8px}.version,.size,footer{color:var(--muted);font-size:13px}p{color:var(--muted);margin:0 0 28px}strong{font-weight:500;color:var(--fg)}ul{list-style:none;margin:0 0 24px;padding:0;border-top:1px solid var(--line)}li{border-bottom:1px solid var(--line)}li a,.row{display:flex;justify-content:space-between;align-items:center;gap:16px;padding:17px 0;text-decoration:none;color:var(--fg)}li a:hover{background:var(--hover)}.off{color:var(--muted)}.size{white-space:nowrap}.size span{margin-left:12px}.flag{font-size:11px;color:var(--warn);border:1px solid currentColor;border-radius:4px;padding:0 5px;margin-left:8px;white-space:nowrap}a{color:inherit;text-underline-offset:4px}a:focus-visible,summary:focus-visible{outline:2px solid var(--fg);outline-offset:5px}.note{font-size:12px;margin-bottom:24px}code{font-size:12px}details{font-size:13px;color:var(--muted);margin-bottom:24px}summary{cursor:pointer;width:fit-content}details p{margin:12px 0 0;white-space:pre-line}details ul{margin-top:12px}details li a,details .row{padding:10px 0}details h3{font-size:13px;font-weight:500;margin:18px 0 0;color:var(--fg)}footer{border-top:1px solid var(--line);padding-top:18px;display:flex;justify-content:space-between;gap:16px;flex-wrap:wrap}@media(prefers-color-scheme:dark){:root{--bg:#181a18;--fg:#e8eae5;--muted:#a4aaa0;--line:#353a33;--hover:#222620;--warn:#f0a840}}@media(max-width:480px){main{margin-top:56px}.size{font-size:12px}li a{gap:10px}}
</style></head><body><main>
<?php if ($current === null): ?>
<header><h1>ICU</h1></header>
<p>Zurzeit ist kein Release veröffentlicht.</p>
<?php else:
    $clients = array_filter($current['files'], fn($f) => $f['kind'] === 'client');
    $servers = array_filter($current['files'], fn($f) => $f['kind'] === 'server');
?>
<header><h1>ICU</h1><span class="version"><?= h($current['version']) ?><?= $current['published'] ? ' · ' . h(substr((string) $current['published'], 0, 10)) : '' ?></span></header>
<p>Client für Stimme und Video.<br>Download wählen und die <strong>.icu-Einladung</strong> vom Admin öffnen.</p>
<ul aria-label="Downloads für 64-Bit-Systeme"><?php foreach ($clients as $f) echo file_row($f, $current['version']); ?></ul>
<p class="note">Alle Downloads für x86_64. Windows 10/11.<br>Fedora: Das Paket heißt <code>icu-client</code>. Alten ICU-Client als RPM <code>icu</code> zuerst entfernen; die Konfiguration bleibt erhalten.</p>
<?php if ($current['notes']): ?>
<details><summary>Neu in <?= h($current['version']) ?></summary><p><?= h($current['notes']) ?></p></details>
<?php endif; ?>
<?php if ($servers): ?>
<details><summary>Server-Pakete</summary><ul><?php foreach ($servers as $f) echo file_row($f, $current['version']); ?></ul></details>
<?php endif; ?>
<?php endif; ?>
<?php if ($older): ?>
<details><summary>Ältere Versionen</summary>
<?php foreach ($older as $r): ?>
<h3><?= h($r['version']) ?><?= $r['published'] ? ' · ' . h(substr((string) $r['published'], 0, 10)) : '' ?><?= $r['sums'] ? ' · <a href="' . h($r['sums']) . '">SHA256SUMS</a>' : '' ?></h3>
<?php if ($r['notes']): ?><p><?= h($r['notes']) ?></p><?php endif; ?>
<ul><?php foreach ($r['files'] as $f) echo file_row($f, $r['version']); ?></ul>
<?php endforeach; ?>
</details>
<?php endif; ?>
<footer><span><?= $serverNote !== '' ? h($serverNote) : '&nbsp;' ?></span><span><?php if ($current && $current['sums']): ?><a href="<?= h($current['sums']) ?>">SHA-256 Prüfsummen</a> · <?php endif; ?><a href="api/v1/releases">API</a></span></footer>
</main></body></html>
